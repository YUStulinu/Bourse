package bourse.risk

import com.sun.net.httpserver.HttpExchange
import com.sun.net.httpserver.HttpServer
import java.io.File
import java.net.InetSocketAddress
import java.util.Properties
import java.util.concurrent.ConcurrentLinkedQueue

/**
 * The risk and clearing service. It reads every event of the engine from the
 * bus, books trades in the ledger, computes margins, blocks accounts that
 * breach (sending a block command to the engine on its own inbound ring),
 * writes the limits file the gateways read, and at the end of the day the
 * net settlement report.
 *
 *   risk --bus run/bus.dat --config config/risk.properties --http 127.0.0.1:8081
 *        --limits run/limits.json --report run/settlement.json
 */
class Service(val bus: Bus, val config: Properties, val limitsPath: String?, val reportPath: String?) {
    val symbols = bus.symbols()
    val ledger = Ledger(symbols)
    val collateral = config.stringPropertyNames().filter { it.startsWith("collateral.") && it != "collateral.default" }
        .associate { it.removePrefix("collateral.").toInt() to (config.getProperty(it).toDouble() * 1e4).toLong() }
    val risk = Risk(ledger, collateral, ((config.getProperty("collateral.default") ?: "1000000").toDouble() * 1e4).toLong())
    val limits = Limits(
        maxOrderQty = config.getProperty("limit.max_order_qty", "100000").toLong(),
        maxOrderNotional = config.getProperty("limit.max_order_notional", "5000000").toDouble(),
        collarPct = config.getProperty("limit.collar_pct", "10").toDouble(),
        maxOpenNotional = config.getProperty("limit.max_open_notional", "50000000").toDouble(),
        maxMsgsPerSec = config.getProperty("limit.max_msgs_per_sec", "1000000").toLong(),
    )
    val lock = Any()
    val commands = ConcurrentLinkedQueue<Pair<Int, Int>>() // (command type, account)
    var events = 0L
    private var limitsWritten = ""

    fun onEvent(seq: Long, e: Event) {
        events++
        if (e.type == Event.EXECUTED) {
            // the resting order's side tells who bought
            val (buyer, seller) = if (e.side == 'S') e.account2 to e.account else e.account to e.account2
            ledger.trade(seq, e.ts, e.symbol, buyer, seller, e.qty, e.price)
        }
        risk.onEvent(e)
    }

    fun limitsJson(): String {
        fun l(x: Limits) = """{"max_order_qty": ${x.maxOrderQty}, "max_order_notional": ${x.maxOrderNotional}, "collar_pct": ${x.collarPct}, "max_open_notional": ${x.maxOpenNotional}, "max_msgs_per_sec": ${x.maxMsgsPerSec}}"""
        return """{"default": ${l(limits)}, "accounts": {}, "blocked": [${risk.blocked.sorted().joinToString(", ")}]}"""
    }

    fun writeLimits() {
        val path = limitsPath ?: return
        val j = limitsJson()
        if (j == limitsWritten) return
        val tmp = File("$path.tmp")
        tmp.writeText(j)
        val dst = File(path)
        if (!tmp.renameTo(dst)) {
            dst.delete(); tmp.renameTo(dst)
        }
        limitsWritten = j
    }

    fun settlementJson(): String = synchronized(lock) {
        val s = ledger.settle()
        val obligations = s.shares.joinToString(",\n  ") { """{"account": ${it.account}, "symbol": "${it.symbol}", "shares": ${it.shares}}""" }
        val cash = s.cash.entries.joinToString(", ") { """"${it.key}": ${it.value / 1e4}""" }
        """{"trades": ${s.grossTrades}, "net_deliveries": ${s.netDeliveries}, "shares_balance": ${s.sharesBalanced}, "cash_balances": ${s.cashBalanced},
 "cash": {$cash},
 "deliveries": [
  $obligations
 ]}"""
    }

    fun accountsJson(): String = synchronized(lock) {
        ledger.accounts().sorted().joinToString(",\n ", "[\n ", "\n]") { a ->
            val r = risk.report(a)
            val pos = r.positions.entries.joinToString(", ") { """"${it.key}": ${it.value}""" }
            """{"account": $a, "collateral": ${r.collateral}, "cash": ${r.cash}, "equity": ${r.equity}, "margin": ${r.margin}, "open_orders": ${r.openOrders}, "realized": ${r.realized}, "unrealized": ${r.unrealized}, "blocked": ${r.blocked}, "positions": {$pos}}"""
        }
    }

    fun serveHttp(addr: String) {
        val (host, port) = addr.split(":").let { it[0] to it[1].toInt() }
        val server = HttpServer.create(InetSocketAddress(host, port), 0)
        fun reply(x: HttpExchange, body: String, code: Int = 200) {
            val b = body.toByteArray()
            x.responseHeaders.add("Content-Type", "application/json")
            x.responseHeaders.add("Access-Control-Allow-Origin", "*")
            x.sendResponseHeaders(code, b.size.toLong())
            x.responseBody.use { it.write(b) }
        }
        fun account(x: HttpExchange) = x.requestURI.query?.split("&")?.firstOrNull { it.startsWith("account=") }?.removePrefix("account=")?.toIntOrNull()
        server.createContext("/api/accounts") { reply(it, accountsJson()) }
        server.createContext("/api/settlement") { reply(it, settlementJson()) }
        server.createContext("/api/limits") { reply(it, synchronized(lock) { limitsJson() }) }
        server.createContext("/api/block") { x ->
            val a = account(x) ?: return@createContext reply(x, """{"error": "account?"}""", 400)
            commands.add(Bus.CMD_BLOCK to a)
            reply(x, """{"blocking": $a}""")
        }
        server.createContext("/api/unblock") { x ->
            val a = account(x) ?: return@createContext reply(x, """{"error": "account?"}""", 400)
            commands.add(Bus.CMD_UNBLOCK to a)
            reply(x, """{"unblocking": $a}""")
        }
        server.start()
    }

    fun run() {
        bus.Consumer().use { consumer ->
            bus.Producer().use { producer ->
                var lastCheck = 0L
                var idle = 0
                writeLimits()
                while (bus.state() == 1) {
                    val n = synchronized(lock) { consumer.poll(4096, ::onEvent) }
                    while (true) {
                        val (cmd, account) = commands.poll() ?: break
                        producer.push(cmd, account)
                    }
                    val now = System.nanoTime()
                    if (now - lastCheck > 100_000_000L) {
                        lastCheck = now
                        consumer.heartbeat()
                        synchronized(lock) {
                            for (a in risk.breaches(ledger.accounts())) {
                                println("risk: account $a breaches its margin (equity ${risk.equity(a) / 1e4}, margin ${risk.margin(a) / 1e4}): blocking")
                                producer.push(Bus.CMD_BLOCK, a)
                                risk.blocked.add(a) // until the engine confirms
                            }
                            writeLimits()
                        }
                    }
                    // risk does not need microseconds: spin briefly, then sleep
                    if (n == 0) {
                        if (++idle > 10_000) Thread.sleep(1) else Thread.onSpinWait()
                    } else idle = 0
                }
                synchronized(lock) { consumer.poll(Int.MAX_VALUE, ::onEvent) }
            }
        }
        reportPath?.let { File(it).writeText(settlementJson()) }
        println("risk: engine stopped after $events events, ${ledger.trades} trades")
    }
}

fun main(args: Array<String>) {
    fun arg(name: String) = args.indexOf(name).takeIf { it >= 0 && it + 1 < args.size }?.let { args[it + 1] }
    val busPath = arg("--bus") ?: "run/bus.dat"
    val config = Properties()
    arg("--config")?.let { p -> File(p).reader().use { config.load(it) } }
    var bus: Bus? = null
    val start = System.currentTimeMillis()
    while (bus == null) {
        try {
            bus = Bus(busPath).takeIf { it.state() == 1 }
        } catch (e: Exception) {
            if (System.currentTimeMillis() - start > 30_000) throw e
        }
        if (bus == null) Thread.sleep(50)
    }
    val s = Service(bus, config, arg("--limits"), arg("--report"))
    arg("--http")?.let { s.serveHttp(it) }
    println("risk: ${s.symbols.size} symbols, http ${arg("--http")}")
    s.run()
    kotlin.system.exitProcess(0)
}
