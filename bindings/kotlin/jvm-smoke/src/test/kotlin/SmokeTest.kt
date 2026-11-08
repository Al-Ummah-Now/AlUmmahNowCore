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

    private fun memoryStorage(): SecureStorage {
        val items = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()
        return object : SecureStorage {
            override fun get(key: String) = items[key]
            override fun set(key: String, value: ByteArray) { items[key] = value }
            override fun delete(key: String) { items.remove(key) }
            override fun list(prefix: String) = items.keys.filter { it.startsWith(prefix) }.sorted()
        }
    }

    @Test fun secureStorageAdapter() {
        val storage = memoryStorage()
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

    @Test fun authManagerSurface() = runBlocking {
        val auth = AuthManager(
            AuthConfig("https://auth.example.invalid", "viewer-mobile", "alummahnow://auth/callback", listOf("openid"), null, null, null),
            memoryStorage(),
        )
        assertTrue(auth.beginLogin().url.contains("code_challenge_method=S256"))
        assertTrue(auth.accounts().isEmpty())
        assertNull(auth.accessToken())
        assertFailsWith<AunException.Unsupported> { auth.beginDeviceLogin() }
        Unit
    }

    @Test fun uploadEngineSurface() {
        val client = ApiClient(ApiClientConfig("http://127.0.0.1:9", "smoke", null, 2000uL, 1u), null)
        val dir = java.nio.file.Files.createTempDirectory("aun-upload").toString()
        val config = uploadConfigDefault()
        assertEquals(3u, config.maxParallel)
        val engine = UploadEngine(client, dir, "u1", config, null)
        assertTrue(engine.list().isEmpty())
        val request = UploadRequest("/nonexistent/clip.mp4", "درس", null, null, null, null, null, null, emptyMap(), emptyMap(), 0)
        assertFailsWith<AunException.InvalidInput> { engine.enqueue(request) }
        engine.setBandwidthLimit(1_000_000uL)
        engine.shutdown()
    }

    @Test fun mediaSurface() = runBlocking {
        assertTrue(edlSchemaVersion() >= 1u)
        assertTrue(transcriptionModelCatalog().any { it.tier == ModelTier.TINY })
        assertTrue(mediaSelectThumbnailTimestamps(60_000uL, emptyList(), thumbnailConfigDefault()).isNotEmpty())
        val vtt = transcriptToWebvtt(
            """{"language":"ar","segments":[{"start_ms":0,"end_ms":1500,"text":"بسم الله","words":[]}]}""",
            subtitleOptionsDefault(),
        )
        assertTrue(vtt.startsWith("WEBVTT"))
        assertFails { mediaProbeJson("/nonexistent.mp4") }
        Unit
    }
}
