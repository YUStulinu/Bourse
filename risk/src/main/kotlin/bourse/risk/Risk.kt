package bourse.risk

import kotlin.math.max

/**
 * Margin and limits. The margin of an account is the worst loss of its
 * positions over a set of price scenarios (each stock moving up or down by
 * a fraction of its price, the stocks together or one at a time), plus a
 * charge for its open orders, which could all fill. An account whose
 * equity (collateral + cash + positions at the last price) falls below its
 * margin is blocked: the engine cancels its orders and refuses new ones,
 * until the account is unblocked.
 */
data class AccountConfig(val account: Int, val collateral: Long)

data class Limits(
    val maxOrderQty: Long = 100_000,
    val maxOrderNotional: Double = 5_000_000.0,
    val collarPct: Double = 10.0,
    val maxOpenNotional: Double = 50_000_000.0,
    val maxMsgsPerSec: Long = 1_000_000,
)

class Risk(
    private val ledger: Ledger,
    private val collateral: Map<Int, Long>,
    private val defaultCollateral: Long = 1_000_000L * 10_000,
    /** price moves of the scenarios, as fractions */
    private val moves: List<Double> = listOf(0.03, 0.06, 0.10),
    /** margin on open orders, as a fraction of their value */
    private val orderMargin: Double = 0.10,
) {
    /** value of open orders per account: (account, symbol, side) -> 1/10000 dollar */
    private val openValue = HashMap<Int, Long>()
    private val orders = HashMap<Long, Triple<Int, Long, Long>>() // id -> (account, price, open qty)
    val blocked = HashSet<Int>()

    fun collateral(account: Int) = collateral[account] ?: defaultCollateral

    fun onEvent(e: Event) {
        when (e.type) {
            Event.ACCEPTED -> setOpen(e.orderId, e.account, e.price, e.qty)
            Event.EXECUTED -> {
                orders[e.orderId]?.let { (a, p, q) -> setOpen(e.orderId, a, p, q - e.qty) }
                orders[e.orderId2]?.let { (a, p, _) -> setOpen(e.orderId2, a, p, e.qty2) }
            }
            Event.CANCELLED -> orders[e.orderId]?.let { (a, p, _) -> setOpen(e.orderId, a, p, e.qty2) }
            Event.REPLACED -> {
                orders[e.orderId]?.let { (a, p, _) -> setOpen(e.orderId, a, p, 0) }
                setOpen(e.orderId2, e.account, e.price, e.qty)
            }
            Event.BLOCKED -> blocked.add(e.account)
            Event.UNBLOCKED -> blocked.remove(e.account)
        }
    }

    private fun setOpen(id: Long, account: Int, price: Long, qty: Long) {
        val old = orders[id]?.third ?: 0L
        openValue[account] = (openValue[account] ?: 0L) + (qty - old) * price
        if (qty <= 0L) orders.remove(id) else orders[id] = Triple(account, price, qty)
    }

    fun openOrdersValue(account: Int) = openValue[account] ?: 0L

    /** Worst loss over the scenarios (1/10000 dollar, >= 0). */
    fun scenarioLoss(account: Int): Long {
        val pos = ledger.positions(account).filterValues { it.shares != 0L }
        if (pos.isEmpty()) return 0
        var worst = 0.0
        for (m in moves) for (dir in listOf(-1.0, 1.0)) {
            // all stocks together
            val all = pos.entries.sumOf { (s, p) -> p.shares * ledger.last[s] * m * dir }
            worst = max(worst, -all)
            // one stock at a time
            for ((s, p) in pos) worst = max(worst, -(p.shares * ledger.last[s] * m * dir))
        }
        return worst.toLong()
    }

    fun margin(account: Int): Long = scenarioLoss(account) + (openOrdersValue(account) * orderMargin).toLong()

    fun equity(account: Int) = ledger.equity(account, collateral(account))

    /** Accounts that should be blocked now (not already blocked). */
    fun breaches(accounts: Collection<Int>): List<Int> = accounts.filter { it !in blocked && equity(it) < margin(it) }

    data class Report(val account: Int, val collateral: Double, val cash: Double, val equity: Double, val margin: Double, val openOrders: Double,
                      val realized: Double, val unrealized: Double, val blocked: Boolean, val positions: Map<String, Long>)

    fun report(account: Int): Report = Report(
        account,
        collateral(account) / 1e4,
        (ledger.cash[account] ?: 0L) / 1e4,
        equity(account) / 1e4,
        margin(account) / 1e4,
        openOrdersValue(account) / 1e4,
        ledger.positions(account).values.sumOf { it.realized } / 1e4,
        ledger.unrealized(account) / 1e4,
        account in blocked,
        ledger.positions(account).filterValues { it.shares != 0L }.mapKeys { (s, _) -> ledger.symbols.getOrElse(s) { "#$s" } }.mapValues { it.value.shares },
    )
}
