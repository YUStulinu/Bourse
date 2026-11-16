package bourse.risk

/**
 * Clearing: the central counterparty steps between buyer and seller of
 * every trade (novation), so each account holds positions against the CCP
 * only. The ledger keeps positions and cash per account; at the end of the
 * day the obligations are netted: per account and stock, the shares to
 * deliver or receive, and per account the cash to pay or receive.
 *
 * Prices are in 1/10000 dollar, cash in 1/10000 dollar (Long is enough for
 * a day of any account here: 9.2e14 dollars).
 */
class Ledger(val symbols: List<String>) {
    class Position {
        var shares = 0L          // net: + long, - short
        var cost = 0L            // cash paid for the open position (average cost basis)
        var realized = 0L        // profit and loss of closed shares
        var bought = 0L
        var sold = 0L
        var buyValue = 0L
        var sellValue = 0L
    }

    data class Fill(val seq: Long, val ts: Long, val symbol: Int, val buyer: Int, val seller: Int, val shares: Long, val price: Long)

    private val positions = HashMap<Int, HashMap<Int, Position>>()
    val cash = HashMap<Int, Long>()
    val fills = ArrayList<Fill>()
    val last = LongArray(symbols.size)
    var trades = 0L
        private set

    fun position(account: Int, symbol: Int): Position = positions.getOrPut(account) { HashMap() }.getOrPut(symbol) { Position() }
    fun accounts(): Set<Int> = positions.keys + cash.keys
    fun positions(account: Int): Map<Int, Position> = positions[account] ?: emptyMap()

    private fun apply(account: Int, symbol: Int, delta: Long, price: Long) {
        val p = position(account, symbol)
        val value = delta * price
        cash[account] = (cash[account] ?: 0L) - value
        if (delta > 0) { p.bought += delta; p.buyValue += value } else { p.sold -= delta; p.sellValue -= value }
        // average cost: shares that close part of the position realize P&L
        if (p.shares == 0L || (p.shares > 0) == (delta > 0)) {
            p.cost += value
        } else {
            val closing = minOf(Math.abs(delta), Math.abs(p.shares))
            // cost is signed like the position (paid for a long, received for a
            // short), so the average cost per share is positive either way
            val avg = p.cost / p.shares
            val sign = if (p.shares > 0) 1 else -1
            p.realized += sign * closing * (price - avg)
            p.cost -= sign * closing * avg
            val rest = Math.abs(delta) - closing
            if (rest > 0) p.cost = (if (delta > 0) 1 else -1) * rest * price
        }
        p.shares += delta
        if (p.shares == 0L) p.cost = 0
    }

    /** A trade: the buyer gets shares and pays, the seller the opposite, both against the CCP. */
    fun trade(seq: Long, ts: Long, symbol: Int, buyer: Int, seller: Int, shares: Long, price: Long) {
        apply(buyer, symbol, shares, price)
        apply(seller, symbol, -shares, price)
        fills.add(Fill(seq, ts, symbol, buyer, seller, shares, price))
        last[symbol] = price
        trades++
    }

    /** Value of an account's positions at the last trade prices, plus its cash. */
    fun equity(account: Int, collateral: Long): Long {
        var v = collateral + (cash[account] ?: 0L)
        for ((s, p) in positions(account)) v += p.shares * last[s]
        return v
    }

    fun unrealized(account: Int): Long = positions(account).entries.sumOf { (s, p) -> p.shares * last[s] - p.cost }

    data class Obligation(val account: Int, val symbol: String, val shares: Long)
    data class Settlement(val shares: List<Obligation>, val cash: Map<Int, Long>, val sharesBalanced: Boolean, val cashBalanced: Boolean, val grossTrades: Long, val netDeliveries: Long)

    /**
     * Net settlement: what each account delivers (negative) or receives
     * (positive) per stock, and its net cash. Every share and every cent
     * bought was sold by someone, so each stock's obligations sum to zero,
     * and so does the cash.
     */
    fun settle(): Settlement {
        val obligations = ArrayList<Obligation>()
        val perSymbol = LongArray(symbols.size)
        for ((a, m) in positions) for ((s, p) in m) {
            val net = p.bought - p.sold
            if (net != 0L) obligations.add(Obligation(a, symbols.getOrElse(s) { "#$s" }, net))
            perSymbol[s] += net
        }
        val c = cash.filterValues { it != 0L }
        return Settlement(
            obligations.sortedWith(compareBy({ it.account }, { it.symbol })),
            c.toSortedMap(),
            perSymbol.all { it == 0L },
            c.values.sum() == 0L,
            fills.size.toLong(),
            obligations.size.toLong(),
        )
    }
}
