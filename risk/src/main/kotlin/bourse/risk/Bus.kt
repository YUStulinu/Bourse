package bourse.risk

import java.io.RandomAccessFile
import java.lang.invoke.MethodHandles
import java.lang.invoke.VarHandle
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.MappedByteBuffer
import java.nio.channels.FileChannel

/** The engine's events (docs/bus.md), the fields risk and clearing need. */
data class Event(
    val type: Int,
    val side: Char,
    val reason: Int,
    val flags: Int,
    val session: Int,
    val session2: Int,
    val symbol: Int,
    val account: Int,
    val orderId: Long,
    val orderId2: Long,
    val price: Long,
    val qty: Long,
    val qty2: Long,
    val account2: Int,
    val ts: Long,
) {
    companion object {
        const val ACCEPTED = 1
        const val REJECTED = 2
        const val EXECUTED = 3
        const val CANCELLED = 4
        const val REPLACED = 5
        const val BOOKED = 6
        const val BLOCKED = 7
        const val UNBLOCKED = 8

        fun read(b: ByteBuffer, o: Int) = Event(
            type = b.get(o).toInt() and 0xFF,
            side = (b.get(o + 1).toInt() and 0xFF).toChar(),
            reason = b.get(o + 2).toInt() and 0xFF,
            flags = b.get(o + 3).toInt() and 0xFF,
            session = b.getShort(o + 4).toInt() and 0xFFFF,
            session2 = b.getShort(o + 6).toInt() and 0xFFFF,
            symbol = b.getInt(o + 8),
            account = b.getInt(o + 12),
            orderId = b.getLong(o + 16),
            orderId2 = b.getLong(o + 24),
            price = b.getLong(o + 32),
            qty = b.getInt(o + 40).toLong() and 0xFFFFFFFFL,
            qty2 = b.getInt(o + 44).toLong() and 0xFFFFFFFFL,
            account2 = b.getInt(o + 48),
            ts = b.getLong(o + 56),
        )

        /** Encode (for tests). */
        fun write(b: ByteBuffer, o: Int, e: Event) {
            b.put(o, e.type.toByte()); b.put(o + 1, e.side.code.toByte()); b.put(o + 2, e.reason.toByte()); b.put(o + 3, e.flags.toByte())
            b.putShort(o + 4, e.session.toShort()); b.putShort(o + 6, e.session2.toShort())
            b.putInt(o + 8, e.symbol); b.putInt(o + 12, e.account)
            b.putLong(o + 16, e.orderId); b.putLong(o + 24, e.orderId2); b.putLong(o + 32, e.price)
            b.putInt(o + 40, e.qty.toInt()); b.putInt(o + 44, e.qty2.toInt()); b.putInt(o + 48, e.account2); b.putInt(o + 52, 0)
            b.putLong(o + 56, e.ts)
        }
    }
}

/**
 * The bus file mapped into memory. Cursors are read and written with
 * acquire/release semantics through VarHandles, as the C++, Rust and Go
 * sides do with their atomics.
 */
class Bus(path: String) : AutoCloseable {
    private val file = RandomAccessFile(path, "rw")
    val buf: MappedByteBuffer = file.channel.map(FileChannel.MapMode.READ_WRITE, 0, file.length()).also { it.order(ByteOrder.LITTLE_ENDIAN) }
    val nIn = buf.getInt(12)
    val inCap = buf.getInt(16)
    val outCap = buf.getInt(20)
    private val outOff = RINGS_OFF + nIn.toLong() * (RING_HEADER + inCap.toLong() * SLOT)

    init {
        require(buf.getLong(0) == MAGIC) { "$path is not a bus file" }
    }

    fun state(): Int = INT.getAcquire(buf, 28) as Int
    fun symbols(): List<String> = (0 until buf.getInt(24)).map { i ->
        val b = ByteArray(8)
        buf.get(SYMBOLS_OFF + i * 16, b)
        String(b, Charsets.US_ASCII).trimEnd()
    }

    fun logCursor(): Long = LONG.getAcquire(buf, outOff.toInt()) as Long

    /** A registered reader of the event log. */
    inner class Consumer : AutoCloseable {
        private val slot: Int
        var cursor: Long
            private set

        init {
            var found = -1
            for (i in 0 until MAX_CONSUMERS) {
                if (INT.compareAndSet(buf, CONSUMERS_OFF + i * 128, 0, 2)) {
                    found = i; break
                }
            }
            require(found >= 0) { "no free consumer slot on the bus" }
            slot = found
            cursor = logCursor()
            val off = CONSUMERS_OFF + slot * 128
            LONG.setRelease(buf, off + 64, cursor)
            LONG.setRelease(buf, off + 8, System.currentTimeMillis() * 1_000_000)
            buf.putInt(off + 4, ProcessHandle.current().pid().toInt())
            INT.setRelease(buf, off, 1)
        }

        /** Hand each new event (up to max) to f; returns how many. */
        fun poll(max: Int, f: (Long, Event) -> Unit): Int {
            val avail = logCursor()
            var n = 0
            while (cursor < avail && n < max) {
                val o = (outOff + RING_HEADER + (cursor and (outCap - 1).toLong()) * SLOT).toInt()
                f(cursor, Event.read(buf, o))
                cursor++; n++
            }
            if (n > 0) LONG.setRelease(buf, CONSUMERS_OFF + slot * 128 + 64, cursor)
            return n
        }

        fun heartbeat() = LONG.setRelease(buf, CONSUMERS_OFF + slot * 128 + 8, System.currentTimeMillis() * 1_000_000)
        fun active(): Boolean = (INT.getAcquire(buf, CONSUMERS_OFF + slot * 128) as Int) == 1
        override fun close() = INT.setRelease(buf, CONSUMERS_OFF + slot * 128, 0)
    }

    /** The producer side of one inbound ring (commands to the engine). */
    inner class Producer : AutoCloseable {
        private val ring: Int
        private var head: Long
        private val off: Int

        init {
            var found = -1
            val pid = ProcessHandle.current().pid().toInt()
            for (i in 0 until nIn) {
                val o = (RINGS_OFF + i.toLong() * (RING_HEADER + inCap.toLong() * SLOT)).toInt()
                if (INT.compareAndSet(buf, o + 8, 0, pid)) {
                    found = i; break
                }
            }
            require(found >= 0) { "no free inbound ring on the bus" }
            ring = found
            off = (RINGS_OFF + ring.toLong() * (RING_HEADER + inCap.toLong() * SLOT)).toInt()
            head = LONG.getAcquire(buf, off) as Long
        }

        val session: Int get() = ring shl 12

        fun push(type: Int, account: Int, symbol: Int = -1) {
            while (head - (LONG.getAcquire(buf, off + 128) as Long) >= inCap) Thread.onSpinWait()
            val o = off + RING_HEADER + ((head and (inCap - 1).toLong()) * SLOT).toInt()
            for (i in 0 until SLOT) buf.put(o + i, 0)
            buf.put(o, type.toByte())
            buf.putShort(o + 4, session.toShort())
            buf.putInt(o + 8, symbol)
            buf.putInt(o + 12, account)
            buf.putLong(o + 56, System.currentTimeMillis() * 1_000_000)
            head++
            LONG.setRelease(buf, off, head)
        }

        override fun close() = INT.setRelease(buf, off + 8, 0)
    }

    override fun close() = file.close()

    companion object {
        const val MAGIC = 0x3142455352554F42L
        const val SYMBOLS_OFF = 4096
        const val CONSUMERS_OFF = 8192
        const val RINGS_OFF = 12288L
        const val RING_HEADER = 256
        const val SLOT = 64
        const val MAX_CONSUMERS = 16
        const val CMD_MASS_CANCEL = 4
        const val CMD_BLOCK = 5
        const val CMD_UNBLOCK = 6
        val LONG: VarHandle = MethodHandles.byteBufferViewVarHandle(LongArray::class.java, ByteOrder.LITTLE_ENDIAN)
        val INT: VarHandle = MethodHandles.byteBufferViewVarHandle(IntArray::class.java, ByteOrder.LITTLE_ENDIAN)

        /** Create an empty bus file with this layout (for tests). */
        fun create(path: String, nIn: Int, inCap: Int, outCap: Int, symbols: List<String>) {
            val size = RINGS_OFF + nIn.toLong() * (RING_HEADER + inCap.toLong() * SLOT) + RING_HEADER + outCap.toLong() * SLOT
            RandomAccessFile(path, "rw").use { f ->
                f.setLength(0); f.setLength(size)
                val b = f.channel.map(FileChannel.MapMode.READ_WRITE, 0, size)
                b.order(ByteOrder.LITTLE_ENDIAN)
                b.putLong(0, MAGIC); b.putInt(8, 1); b.putInt(12, nIn); b.putInt(16, inCap); b.putInt(20, outCap)
                b.putInt(24, symbols.size); b.putInt(28, 1)
                symbols.forEachIndexed { i, s ->
                    val name = s.padEnd(8).take(8).toByteArray(Charsets.US_ASCII)
                    b.put(SYMBOLS_OFF + i * 16, name)
                    b.putInt(SYMBOLS_OFF + i * 16 + 8, 100); b.putInt(SYMBOLS_OFF + i * 16 + 12, 100)
                }
                b.force()
            }
        }
    }
}
