import kotlinx.coroutines.runBlocking
import net.alummahnow.core.*
import java.util.Collections
import kotlin.test.*

/** Proves sync calls, async calls, errors and callbacks cross the FFI boundary from Kotlin. */
class SmokeTest {
    @Test fun version() = assertTrue(coreVersion().isNotEmpty())

    @Test fun asyncEcho() = runBlocking {
        assertEquals("مالس", selftestAsyncEcho("سلام", 5uL))
    }

    @Test fun errorsMapToAunException() {
        val e = assertFailsWith<AunException.RateLimited> { selftestThrow("rate_limited") }
        assertEquals(1500uL, e.retryAfterMs)
        assertFailsWith<AunException.Unauthorized> { selftestThrow("unauthorized") }
    }

    @Test fun callbacks() = runBlocking {
        val seen = Collections.synchronizedList(mutableListOf<UInt>())
        val listener = object : SelfTestListener {
            override fun onEvent(index: UInt, message: String) { seen.add(index) }
        }
        assertEquals(3u, selftestCallbacks(listener, 3u))
        assertEquals(listOf(0u, 1u, 2u), seen.toList())
    }

    @Test fun secureStorageAdapter() {
        val items = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()
        val storage = object : SecureStorage {
            override fun get(key: String) = items[key]
            override fun set(key: String, value: ByteArray) { items[key] = value }
            override fun delete(key: String) { items.remove(key) }
            override fun list(prefix: String) = items.keys.filter { it.startsWith(prefix) }.sorted()
        }
        assertEquals("سلام", String(selftestSecureStorage(storage), Charsets.UTF_8))
    }

    @Test fun connectivityAdapter() {
        val c = object : Connectivity { override fun current() = NetworkState(online = false, metered = true) }
        assertEquals(NetworkState(online = false, metered = true), selftestConnectivity(c))
    }

    @Test fun apiClientSurface() = runBlocking {
        assertTrue(apiOperations().any { it.path == "/api/videos/upload/initiate" })
        val client = ApiClient(ApiClientConfig("http://127.0.0.1:9", "smoke", "ar", 2000uL, 1u), null)
        val e = assertFails { client.request(ApiCall("get_GetI18nConfigEndpoint", emptyMap(), emptyMap(), emptyMap(), null, null)) }
        assertTrue(e is AunException.Network || e is AunException.InvalidInput, "$e")
    }
}
