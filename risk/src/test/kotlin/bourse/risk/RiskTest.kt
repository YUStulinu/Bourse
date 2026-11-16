package bourse.risk

import java.io.File
import java.nio.ByteOrder
import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class LedgerTest {
    private val px = 10_000L // one dollar

    @Test
    fun positionsCashAndPnl() {
        val l = Ledger(listOf("AAPL"))
        l.trade(1, 0, 0, buyer = 1, seller = 2, shares = 100, price = 150 * px)
        l.trade(2, 0, 0, buyer = 2, seller = 1, shares = 40, price = 155 * px)
        val a = l.position(1, 0)
        assertEquals(60, a.shares)
        assertEquals(40L * 5 * px, a.realized) // sold 40 bought at 150 for 155
        assertEquals(-100L * 150 * px + 40L * 155 * px, l.cash[1])
        assertEquals(-60, l.position(2, 0).shares)
        assertEquals(-40L * 5 * px, l.position(2, 0).realized)
        // unrealized at the last price (155): 60 shares bought at 150
        assertEquals(60L * 5 * px, l.unrealized(1))
        assertEquals(-60L * 5 * px, l.unrealized(2))
        // a position that flips from long to short
        l.trade(3, 0, 0, buyer = 2, seller = 1, shares = 100, price = 160 * px)
        assertEquals(-40, l.position(1, 0).shares)
        assertEquals(40L * 5 * px + 60L * 10 * px, l.position(1, 0).realized)
        assertEquals(-40L * 160 * px, l.position(1, 0).cost)
    }

    @Test
    fun settlementNetsAndBalances() {
        val rng = Random(7)
        val l = Ledger(listOf("A", "B", "C"))
        repeat(20_000) { i ->
            val b = rng.nextInt(1, 30)
            var s = rng.nextInt(1, 30)
            if (s == b) s = b % 29 + 1
            l.trade(i.toLong(), 0, rng.nextInt(3), b, s, rng.nextLong(1, 10) * 100, rng.nextLong(90, 110) * px)
        }
        val st = l.settle()
        assertTrue(st.sharesBalanced, "deliveries of every stock sum to zero")
        assertTrue(st.cashBalanced, "cash sums to zero")
        assertEquals(20_000, st.grossTrades)
        assertTrue(st.netDeliveries <= 29 * 3, "netting leaves at most one obligation per account and stock")
        // positions agree with the obligations
        for (o in st.shares) assertEquals(l.position(o.account, listOf("A", "B", "C").indexOf(o.symbol)).shares, o.shares)
    }
}

class MarginTest {
    @Test
    fun scenariosAndBreaches() {
        val l = Ledger(listOf("X", "Y"))
        val r = Risk(l, mapOf(1 to 20_000L * 10_000, 2 to 1_000_000L * 10_000), moves = listOf(0.1))
        // account 1 buys 1000 X at $100 from account 2
        l.trade(1, 0, 0, buyer = 1, seller = 2, shares = 1000, price = 100L * 10_000)
        // worst case: X down 10% = $10,000 loss
        assertEquals(10_000L * 10_000, r.scenarioLoss(1))
        assertEquals(10_000L * 10_000, r.scenarioLoss(2)) // the short loses if X goes up
        // equity of 1: 20,000 collateral - 100,000 cash + 100,000 shares = 20,000 > 10,000
        assertTrue(r.breaches(listOf(1, 2)).isEmpty())
        // the price falls to $85: equity 5,000 < margin 8,500
        l.trade(2, 0, 0, buyer = 2, seller = 2, shares = 1, price = 85L * 10_000)
        assertEquals(listOf(1), r.breaches(listOf(1, 2)))
        // open orders add to the margin
        r.onEvent(Event(Event.ACCEPTED, 'B', 0, 0, 0, 0, 1, 2, 77, 0, 50L * 10_000, 1000, 0, 0, 0))
        assertEquals(50_000L * 10_000, r.openOrdersValue(2))
        r.onEvent(Event(Event.CANCELLED, 'B', 1, 0, 0, 0, 1, 2, 77, 0, 50L * 10_000, 1000, 0, 0, 0))
        assertEquals(0, r.openOrdersValue(2))
    }
}

class BusTest {
    @Test
    fun readsEventsAndWritesCommands() {
        val f = File.createTempFile("bourse-bus", ".dat")
        f.deleteOnExit()
        Bus.create(f.path, nIn = 2, inCap = 8, outCap = 16, symbols = listOf("AAPL", "MSFT"))
        Bus(f.path).use { bus ->
            assertEquals(listOf("AAPL", "MSFT"), bus.symbols())
            val c = bus.Consumer()
            // play the engine: write 40 events into the log (it wraps around)
            val outOff = (Bus.RINGS_OFF + 2L * (Bus.RING_HEADER + 8L * Bus.SLOT)).toInt()
            val seen = ArrayList<Long>()
            for (i in 0 until 40) {
                val e = Event(Event.EXECUTED, 'S', 0, 0, 1, 2, 1, 3, 1000L + i, 2000L + i, 1_500_000, 100, 0, 4, i.toLong())
                Event.write(bus.buf, outOff + Bus.RING_HEADER + (i % 16) * Bus.SLOT, e)
                Bus.LONG.setRelease(bus.buf, outOff, (i + 1).toLong())
                c.poll(100) { seq, ev -> assertEquals(seq + 1000, ev.orderId); assertEquals(4, ev.account2); seen.add(seq) }
            }
            assertEquals((0L until 40L).toList(), seen)
            // the consumer published its cursor where the engine looks for it
            assertEquals(40L, bus.buf.order(ByteOrder.LITTLE_ENDIAN).getLong(Bus.CONSUMERS_OFF + 64))
            val p = bus.Producer()
            p.push(Bus.CMD_BLOCK, 42)
            val ring0 = Bus.RINGS_OFF.toInt()
            assertEquals(1L, bus.buf.getLong(ring0))
            assertEquals(Bus.CMD_BLOCK, bus.buf.get(ring0 + Bus.RING_HEADER).toInt())
            assertEquals(42, bus.buf.getInt(ring0 + Bus.RING_HEADER + 12))
            p.close(); c.close()
        }
    }
}
