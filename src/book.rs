//! The order book: a fixed price ladder per side, an intrusive FIFO per price level,
//! a slab of order nodes, and an id → node index.
//!
//! | Operation | Cost |
//! |---|---|
//! | best bid / best ask | O(1) (cached) |
//! | add resting order | O(1) amortized (O(1) hash insert, append to level tail) |
//! | cancel by id | O(1) hash lookup + O(1) unlink; O(L/64) to find the next best level if the best level empties (L = levels in band) |
//! | fill against front of best level | O(1); same O(L/64) re-scan when a level empties |
//!
//! All storage is allocated up front from [`BookConfig`]. After construction the book
//! never grows: the node pool is bounded (`BookFull` rejection), the ladder has one
//! slot per tick in the band, and the id index is sized at twice the node pool so the
//! hash table can always rehash in place instead of reallocating.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::types::{OrderId, Price, Qty, Side};

const NIL: u32 = u32::MAX;
/// Upper bound on ladder size, to keep memory bounded (≈ 24 B × 2 × 4 Mi = 192 MiB).
pub const MAX_LEVELS: u64 = 1 << 22;
/// Upper bound on the order pool (node indices are `u32`, `NIL` is reserved).
pub const MAX_ORDERS: u32 = (1 << 30) - 1;

/// Static book parameters. Validated by [`BookConfig::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookConfig {
    /// Price increment. Every price must be a multiple of it.
    pub tick: Price,
    /// Lowest accepted limit price (inclusive, multiple of `tick`).
    pub min_price: Price,
    /// Highest accepted limit price (inclusive, multiple of `tick`).
    pub max_price: Price,
    /// Maximum number of simultaneously resting orders.
    pub max_orders: u32,
}

/// An invalid [`BookConfig`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Tick was zero.
    #[error("tick must be > 0")]
    ZeroTick,
    /// Band bounds are not tick multiples or are inverted.
    #[error(
        "price band [{min}, {max}] must satisfy min <= max and both be multiples of tick {tick}"
    )]
    BadBand {
        /// min_price
        min: Price,
        /// max_price
        max: Price,
        /// tick
        tick: Price,
    },
    /// Band too wide for the fixed ladder.
    #[error("price band has {0} levels; maximum is {MAX_LEVELS}")]
    TooManyLevels(u64),
    /// Order pool size out of range.
    #[error("max_orders must be in 1..={MAX_ORDERS}, got {0}")]
    BadMaxOrders(u32),
}

impl BookConfig {
    /// Checks invariants and returns the number of price levels.
    pub fn validate(&self) -> Result<u64, ConfigError> {
        if self.tick == 0 {
            return Err(ConfigError::ZeroTick);
        }
        if self.min_price > self.max_price
            || self.min_price % self.tick != 0
            || self.max_price % self.tick != 0
        {
            return Err(ConfigError::BadBand {
                min: self.min_price,
                max: self.max_price,
                tick: self.tick,
            });
        }
        let levels = (self.max_price - self.min_price) / self.tick + 1;
        if levels > MAX_LEVELS {
            return Err(ConfigError::TooManyLevels(levels));
        }
        if self.max_orders == 0 || self.max_orders > MAX_ORDERS {
            return Err(ConfigError::BadMaxOrders(self.max_orders));
        }
        Ok(levels)
    }
}

/// A tiny non-cryptographic hasher for `u64` ids (splitmix64 finalizer).
///
/// Not HashDoS-resistant: an adversary choosing ids could degrade lookups. Acceptable
/// for a research engine with trusted input; see `docs/LIMITATIONS.md`.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        let mut z = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8) | u64::from(b);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

#[derive(Debug, Clone, Copy)]
struct Node {
    id: OrderId,
    qty: Qty,
    level: u32,
    side: Side,
    prev: u32,
    /// Next in level FIFO, or next free node when on the free list.
    next: u32,
}

#[derive(Debug, Clone, Copy)]
struct Level {
    head: u32,
    tail: u32,
    qty: Qty,
    count: u32,
}

const EMPTY_LEVEL: Level = Level {
    head: NIL,
    tail: NIL,
    qty: 0,
    count: 0,
};

/// One side of the book: a level per tick plus a bitset of non-empty levels.
#[derive(Debug)]
struct Ladder {
    levels: Box<[Level]>,
    nonempty: Box<[u64]>,
    best: u32,
    side: Side,
}

impl Ladder {
    fn new(n: usize, side: Side) -> Self {
        Self {
            levels: vec![EMPTY_LEVEL; n].into_boxed_slice(),
            nonempty: vec![0u64; n.div_ceil(64)].into_boxed_slice(),
            best: NIL,
            side,
        }
    }

    fn set(&mut self, i: u32) {
        self.nonempty[(i / 64) as usize] |= 1u64 << (i % 64);
        let better = match self.side {
            Side::Buy => self.best == NIL || i > self.best,
            Side::Sell => self.best == NIL || i < self.best,
        };
        if better {
            self.best = i;
        }
    }

    fn clear(&mut self, i: u32) {
        self.nonempty[(i / 64) as usize] &= !(1u64 << (i % 64));
        if i == self.best {
            self.best = match self.side {
                Side::Buy => self.prev_set(i),
                Side::Sell => self.next_set(i),
            };
        }
    }

    /// Highest set index strictly below `i`, or NIL.
    fn prev_set(&self, i: u32) -> u32 {
        if i == 0 {
            return NIL;
        }
        let i = i - 1;
        let mut w = (i / 64) as usize;
        let bit = i % 64;
        let mask = if bit == 63 {
            u64::MAX
        } else {
            (1u64 << (bit + 1)) - 1
        };
        let mut word = self.nonempty[w] & mask;
        loop {
            if word != 0 {
                return (w as u32) * 64 + (63 - word.leading_zeros());
            }
            if w == 0 {
                return NIL;
            }
            w -= 1;
            word = self.nonempty[w];
        }
    }

    /// Lowest set index strictly above `i`, or NIL.
    fn next_set(&self, i: u32) -> u32 {
        let i = i + 1;
        let mut w = (i / 64) as usize;
        if w >= self.nonempty.len() {
            return NIL;
        }
        let mut word = self.nonempty[w] & (u64::MAX << (i % 64));
        loop {
            if word != 0 {
                return (w as u32) * 64 + word.trailing_zeros();
            }
            w += 1;
            if w >= self.nonempty.len() {
                return NIL;
            }
            word = self.nonempty[w];
        }
    }
}

/// A resting order as seen from outside the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingOrder {
    /// Order id.
    pub id: OrderId,
    /// Side.
    pub side: Side,
    /// Price.
    pub price: Price,
    /// Remaining quantity.
    pub qty: Qty,
}

/// The front (oldest) order at the best level of one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Front {
    pub(crate) node: u32,
    pub(crate) id: OrderId,
    pub(crate) qty: Qty,
    pub(crate) level: u32,
}

/// Limit order book with preallocated, bounded storage.
#[derive(Debug)]
pub struct OrderBook {
    cfg: BookConfig,
    bids: Ladder,
    asks: Ladder,
    nodes: Box<[Node]>,
    free: u32,
    index: HashMap<OrderId, u32, BuildHasherDefault<IdHasher>>,
}

impl OrderBook {
    /// Builds an empty book, allocating all storage now.
    pub fn new(cfg: BookConfig) -> Result<Self, ConfigError> {
        let levels = cfg.validate()? as usize;
        let n = cfg.max_orders as usize;
        let nodes = (0..n)
            .map(|i| Node {
                id: 0,
                qty: 0,
                level: NIL,
                side: Side::Buy,
                prev: NIL,
                next: if i + 1 < n { (i + 1) as u32 } else { NIL },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut index = HashMap::with_hasher(BuildHasherDefault::default());
        index.reserve(n.saturating_mul(2));
        Ok(Self {
            cfg,
            bids: Ladder::new(levels, Side::Buy),
            asks: Ladder::new(levels, Side::Sell),
            nodes,
            free: 0,
            index,
        })
    }

    /// The configuration this book was built with.
    pub fn config(&self) -> &BookConfig {
        &self.cfg
    }

    /// Number of resting orders.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether no orders rest.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Whether another order can rest.
    pub fn has_capacity(&self) -> bool {
        self.free != NIL
    }

    /// Whether `id` is resting.
    pub fn contains(&self, id: OrderId) -> bool {
        self.index.contains_key(&id)
    }

    /// Ladder index for a validated price.
    pub(crate) fn level_of(&self, price: Price) -> u32 {
        ((price - self.cfg.min_price) / self.cfg.tick) as u32
    }

    /// Price of a ladder index.
    pub(crate) fn price_of(&self, level: u32) -> Price {
        self.cfg.min_price + u64::from(level) * self.cfg.tick
    }

    fn ladder(&self, side: Side) -> &Ladder {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn ladder_mut(&mut self, side: Side) -> &mut Ladder {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    /// Best price and total quantity on `side`.
    pub fn best(&self, side: Side) -> Option<(Price, Qty)> {
        let l = self.ladder(side);
        (l.best != NIL).then(|| (self.price_of(l.best), l.levels[l.best as usize].qty))
    }

    /// Best bid `(price, qty)`.
    pub fn best_bid(&self) -> Option<(Price, Qty)> {
        self.best(Side::Buy)
    }

    /// Best ask `(price, qty)`.
    pub fn best_ask(&self) -> Option<(Price, Qty)> {
        self.best(Side::Sell)
    }

    /// Looks up a resting order.
    pub fn order(&self, id: OrderId) -> Option<RestingOrder> {
        let &n = self.index.get(&id)?;
        let node = &self.nodes[n as usize];
        Some(RestingOrder {
            id,
            side: node.side,
            price: self.price_of(node.level),
            qty: node.qty,
        })
    }

    pub(crate) fn front(&self, side: Side) -> Option<Front> {
        let l = self.ladder(side);
        if l.best == NIL {
            return None;
        }
        let node = l.levels[l.best as usize].head;
        let n = &self.nodes[node as usize];
        Some(Front {
            node,
            id: n.id,
            qty: n.qty,
            level: l.best,
        })
    }

    /// Appends a new resting order. Caller has validated price/id; fails only when
    /// the pool is exhausted.
    pub(crate) fn insert(
        &mut self,
        id: OrderId,
        side: Side,
        level: u32,
        qty: Qty,
    ) -> Result<(), crate::types::RejectReason> {
        let n = self.free;
        if n == NIL {
            return Err(crate::types::RejectReason::BookFull);
        }
        self.free = self.nodes[n as usize].next;
        let ladder = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let lvl = &mut ladder.levels[level as usize];
        let old_tail = lvl.tail;
        self.nodes[n as usize] = Node {
            id,
            qty,
            level,
            side,
            prev: old_tail,
            next: NIL,
        };
        if old_tail == NIL {
            lvl.head = n;
        } else {
            self.nodes[old_tail as usize].next = n;
        }
        lvl.tail = n;
        lvl.qty += qty;
        lvl.count += 1;
        if lvl.count == 1 {
            ladder.set(level);
        }
        self.index.insert(id, n);
        Ok(())
    }

    /// Reduces the front order at the best level of `side` by `qty`
    /// (`qty <= front.qty`), removing it if it reaches zero.
    pub(crate) fn fill_front(&mut self, side: Side, front: Front, qty: Qty) {
        let node = &mut self.nodes[front.node as usize];
        node.qty -= qty;
        let done = node.qty == 0;
        self.ladder_mut(side).levels[front.level as usize].qty -= qty;
        if done {
            self.unlink(front.node);
        }
    }

    /// Removes a resting order by id, returning its remaining quantity.
    pub(crate) fn remove(&mut self, id: OrderId) -> Option<Qty> {
        let &n = self.index.get(&id)?;
        let qty = self.nodes[n as usize].qty;
        let side = self.nodes[n as usize].side;
        let level = self.nodes[n as usize].level;
        self.ladder_mut(side).levels[level as usize].qty -= qty;
        self.unlink(n);
        Some(qty)
    }

    /// Unlinks node `n` from its level and returns it to the free list.
    /// The level's `qty` must already exclude the node's remaining quantity.
    fn unlink(&mut self, n: u32) {
        let Node {
            id,
            level,
            side,
            prev,
            next,
            ..
        } = self.nodes[n as usize];
        if prev != NIL {
            self.nodes[prev as usize].next = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        }
        let ladder = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let lvl = &mut ladder.levels[level as usize];
        if lvl.head == n {
            lvl.head = next;
        }
        if lvl.tail == n {
            lvl.tail = prev;
        }
        lvl.count -= 1;
        if lvl.count == 0 {
            ladder.clear(level);
        }
        self.index.remove(&id);
        let node = &mut self.nodes[n as usize];
        node.level = NIL;
        node.prev = NIL;
        node.next = self.free;
        self.free = n;
    }

    /// Visits resting orders in priority order: bids best→worst, then asks best→worst,
    /// FIFO within each level. Does not allocate.
    pub fn for_each_order(&self, mut f: impl FnMut(RestingOrder)) {
        for side in [Side::Buy, Side::Sell] {
            let l = self.ladder(side);
            let mut lv = l.best;
            while lv != NIL {
                let mut n = l.levels[lv as usize].head;
                while n != NIL {
                    let node = &self.nodes[n as usize];
                    f(RestingOrder {
                        id: node.id,
                        side,
                        price: self.price_of(lv),
                        qty: node.qty,
                    });
                    n = node.next;
                }
                lv = match side {
                    Side::Buy => l.prev_set(lv),
                    Side::Sell => l.next_set(lv),
                };
            }
        }
    }

    /// Aggregated depth for `side`, best first: `(price, total qty, order count)`.
    /// Allocates; intended for display and tests.
    pub fn depth(&self, side: Side) -> Vec<(Price, Qty, u32)> {
        let l = self.ladder(side);
        let mut out = Vec::new();
        let mut lv = l.best;
        while lv != NIL {
            let level = &l.levels[lv as usize];
            out.push((self.price_of(lv), level.qty, level.count));
            lv = match side {
                Side::Buy => l.prev_set(lv),
                Side::Sell => l.next_set(lv),
            };
        }
        out
    }

    /// Deterministic 64-bit FNV-1a fingerprint of the full book state (every resting
    /// order's side, price, id, and quantity in priority order). Not cryptographic:
    /// it detects accidental divergence in replay, not tampering.
    pub fn state_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |x: u64| {
            for b in x.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        self.for_each_order(|o| {
            mix(match o.side {
                Side::Buy => 1,
                Side::Sell => 2,
            });
            mix(o.price);
            mix(o.id);
            mix(o.qty);
        });
        h
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn cfg() -> BookConfig {
        BookConfig {
            tick: 5,
            min_price: 100,
            max_price: 1000,
            max_orders: 4,
        }
    }

    #[test]
    fn config_validation() {
        assert_eq!(
            BookConfig { tick: 0, ..cfg() }.validate(),
            Err(ConfigError::ZeroTick)
        );
        assert!(matches!(
            BookConfig {
                min_price: 101,
                ..cfg()
            }
            .validate(),
            Err(ConfigError::BadBand { .. })
        ));
        assert_eq!(
            BookConfig {
                max_orders: 0,
                ..cfg()
            }
            .validate(),
            Err(ConfigError::BadMaxOrders(0))
        );
        assert!(matches!(
            BookConfig {
                tick: 1,
                min_price: 0,
                max_price: MAX_LEVELS,
                max_orders: 1
            }
            .validate(),
            Err(ConfigError::TooManyLevels(_))
        ));
        assert_eq!(cfg().validate(), Ok(181));
    }

    #[test]
    fn best_tracks_inserts_and_removals() {
        let mut b = OrderBook::new(cfg()).unwrap();
        let l = |p| b.level_of(p);
        let (l100, l200, l300) = (l(100), l(200), l(300));
        b.insert(1, Side::Buy, l200, 5).unwrap();
        b.insert(2, Side::Buy, l300, 7).unwrap();
        b.insert(3, Side::Buy, l100, 1).unwrap();
        assert_eq!(b.best_bid(), Some((300, 7)));
        b.remove(2).unwrap();
        assert_eq!(b.best_bid(), Some((200, 5)));
        b.remove(1).unwrap();
        assert_eq!(b.best_bid(), Some((100, 1)));
        b.remove(3).unwrap();
        assert_eq!(b.best_bid(), None);
        assert!(b.is_empty());
    }

    #[test]
    fn pool_is_bounded_and_recycled() {
        let mut b = OrderBook::new(cfg()).unwrap();
        let lv = b.level_of(500);
        for id in 0..4 {
            b.insert(id, Side::Sell, lv, 1).unwrap();
        }
        assert!(!b.has_capacity());
        assert_eq!(
            b.insert(9, Side::Sell, lv, 1),
            Err(crate::types::RejectReason::BookFull)
        );
        b.remove(2).unwrap();
        b.insert(9, Side::Sell, lv, 1).unwrap();
        let mut ids = Vec::new();
        b.for_each_order(|o| ids.push(o.id));
        assert_eq!(ids, vec![0, 1, 3, 9], "FIFO preserved after middle removal");
    }

    #[test]
    fn bitset_search_crosses_word_boundaries() {
        let mut l = Ladder::new(200, Side::Sell);
        l.set(3);
        l.set(64);
        l.set(130);
        assert_eq!(l.best, 3);
        l.clear(3);
        assert_eq!(l.best, 64);
        l.clear(64);
        assert_eq!(l.best, 130);
        let mut b = Ladder::new(200, Side::Buy);
        b.set(0);
        b.set(63);
        b.set(199);
        assert_eq!(b.best, 199);
        b.clear(199);
        assert_eq!(b.best, 63);
        b.clear(63);
        assert_eq!(b.best, 0);
        b.clear(0);
        assert_eq!(b.best, NIL);
    }
}
