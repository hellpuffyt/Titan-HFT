//! Plain-data types shared by the book, the engine, and the journal.
//!
//! Everything here is `Copy` so commands and events can travel through the SPSC queue
//! and be written into reusable buffers without heap allocation.

use core::fmt;

/// Client-assigned order identifier. Must be unique among *resting* orders.
pub type OrderId = u64;
/// Price in integer price units (e.g. cents). Must be a multiple of the tick size.
pub type Price = u64;
/// Quantity in integer lots.
pub type Qty = u64;

/// Largest accepted order quantity (2^32). With at most 2^30 resting orders a price
/// level's total stays below 2^62, so level sums can never overflow `u64`.
pub const MAX_ORDER_QTY: Qty = 1 << 32;

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Bid.
    Buy,
    /// Ask.
    Sell,
}

impl Side {
    /// The side this order trades against.
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// One-letter journal code.
    pub fn code(self) -> char {
        match self {
            Side::Buy => 'B',
            Side::Sell => 'S',
        }
    }
}

/// An instruction to the matching engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Limit order: match what crosses, rest the remainder at `price`.
    Limit {
        /// Order id.
        id: OrderId,
        /// Side.
        side: Side,
        /// Limit price.
        price: Price,
        /// Quantity.
        qty: Qty,
    },
    /// Market order with immediate-or-cancel semantics: match against the book,
    /// cancel any unfilled remainder. Never rests.
    Market {
        /// Order id.
        id: OrderId,
        /// Side.
        side: Side,
        /// Quantity.
        qty: Qty,
    },
    /// Cancel a resting order.
    Cancel {
        /// Id of the resting order.
        id: OrderId,
    },
}

impl Command {
    /// The order id this command refers to.
    pub fn id(&self) -> OrderId {
        match *self {
            Command::Limit { id, .. } | Command::Market { id, .. } | Command::Cancel { id } => id,
        }
    }
}

/// Why a command was rejected. Rejections never change book state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// Quantity was zero.
    ZeroQty,
    /// Quantity above [`MAX_ORDER_QTY`].
    QtyTooLarge,
    /// Limit price was not a multiple of the tick size.
    BadTick,
    /// Limit price outside the configured price band.
    PriceOutOfBand,
    /// An order with this id is already resting.
    DuplicateId,
    /// Cancel for an id that is not resting.
    UnknownOrder,
    /// The order-node pool is exhausted (bounded capacity).
    BookFull,
}

impl RejectReason {
    /// Stable journal token.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::ZeroQty => "zero_qty",
            RejectReason::QtyTooLarge => "qty_too_large",
            RejectReason::BadTick => "bad_tick",
            RejectReason::PriceOutOfBand => "price_out_of_band",
            RejectReason::DuplicateId => "duplicate_id",
            RejectReason::UnknownOrder => "unknown_order",
            RejectReason::BookFull => "book_full",
        }
    }

    /// Parse a journal token.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "zero_qty" => RejectReason::ZeroQty,
            "qty_too_large" => RejectReason::QtyTooLarge,
            "bad_tick" => RejectReason::BadTick,
            "price_out_of_band" => RejectReason::PriceOutOfBand,
            "duplicate_id" => RejectReason::DuplicateId,
            "unknown_order" => RejectReason::UnknownOrder,
            "book_full" => RejectReason::BookFull,
            _ => return None,
        })
    }
}

/// Why an order left the book without filling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelReason {
    /// Explicit cancel command.
    User,
    /// Unfilled remainder of a market (IOC) order.
    Ioc,
}

impl CancelReason {
    /// Stable journal token.
    pub fn as_str(self) -> &'static str {
        match self {
            CancelReason::User => "user",
            CancelReason::Ioc => "ioc",
        }
    }
}

/// An output of the matching engine. The event stream is a pure function of the
/// command stream and the [`crate::engine::EngineConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The order passed validation.
    Accepted {
        /// Order id.
        id: OrderId,
    },
    /// The command was rejected; no state changed.
    Rejected {
        /// Order id from the command.
        id: OrderId,
        /// Reason.
        reason: RejectReason,
    },
    /// A trade between an incoming (taker) and a resting (maker) order.
    Trade {
        /// Incoming order.
        taker: OrderId,
        /// Resting order.
        maker: OrderId,
        /// Execution price — always the maker's price.
        price: Price,
        /// Executed quantity.
        qty: Qty,
    },
    /// Quantity removed without trading.
    Cancelled {
        /// Order id.
        id: OrderId,
        /// Quantity that was removed.
        qty: Qty,
        /// Reason.
        reason: CancelReason,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Event::Accepted { id } => write!(f, "ACK {id}"),
            Event::Rejected { id, reason } => write!(f, "REJ {id} {}", reason.as_str()),
            Event::Trade {
                taker,
                maker,
                price,
                qty,
            } => write!(f, "TRADE {taker} {maker} {price} {qty}"),
            Event::Cancelled { id, qty, reason } => {
                write!(f, "CXL {id} {qty} {}", reason.as_str())
            }
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Command::Limit {
                id,
                side,
                price,
                qty,
            } => write!(f, "L {} {id} {price} {qty}", side.code()),
            Command::Market { id, side, qty } => write!(f, "M {} {id} {qty}", side.code()),
            Command::Cancel { id } => write!(f, "C {id}"),
        }
    }
}

/// Receives engine events. Implement this to avoid buffering (e.g. write straight to
/// a journal, a counter, or a hash) — the engine itself never allocates for events.
pub trait EventSink {
    /// Called once per event, in order.
    fn emit(&mut self, event: Event);
}

impl EventSink for Vec<Event> {
    #[inline]
    fn emit(&mut self, event: Event) {
        self.push(event);
    }
}

/// A sink that only counts events and traded quantity. Never allocates.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CountingSink {
    /// Total events seen.
    pub events: u64,
    /// Number of trades.
    pub trades: u64,
    /// Sum of traded quantity.
    pub traded_qty: u64,
}

impl EventSink for CountingSink {
    #[inline]
    fn emit(&mut self, event: Event) {
        self.events += 1;
        if let Event::Trade { qty, .. } = event {
            self.trades += 1;
            self.traded_qty += qty;
        }
    }
}
