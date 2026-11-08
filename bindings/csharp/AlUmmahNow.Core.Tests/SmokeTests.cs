using System.Collections.Concurrent;
using System.Text;
using AlUmmahNow.Core;
using Xunit;

namespace AlUmmahNow.Core.Tests;

/// Proves sync calls, async calls, errors and callbacks cross the FFI boundary from C#.
public class SmokeTests
{
    [Fact]
    public void Version() => Assert.False(string.IsNullOrEmpty(AunCoreMethods.CoreVersion()));

    [Fact]
    public async Task AsyncEcho() =>
        Assert.Equal("مالس", await AunCoreMethods.SelftestAsyncEcho("سلام", 5));

    [Fact]
    public void ErrorsMapToAunException()
    {
        var e = Assert.Throws<AunException.RateLimited>(() => AunCoreMethods.SelftestThrow("rate_limited"));
        Assert.Equal(1500UL, e.retryAfterMs);
        Assert.Throws<AunException.Unauthorized>(() => AunCoreMethods.SelftestThrow("unauthorized"));
    }

    private sealed class Listener : SelfTestListener
    {
        public readonly ConcurrentQueue<uint> Seen = new();
        public void OnEvent(uint index, string message) => Seen.Enqueue(index);
    }

    [Fact]
    public async Task Callbacks()
    {
        var l = new Listener();
        Assert.Equal(3U, await AunCoreMethods.SelftestCallbacks(l, 3));
        Assert.Equal(new uint[] { 0, 1, 2 }, l.Seen.ToArray());
    }

    private sealed class MemoryStorage : SecureStorage
    {
        private readonly ConcurrentDictionary<string, byte[]> _items = new();
        public byte[]? Get(string key) => _items.TryGetValue(key, out var v) ? v : null;
        public void Set(string key, byte[] value) => _items[key] = value;
        public void Delete(string key) => _items.TryRemove(key, out _);
        public string[] List(string prefix) => _items.Keys.Where(k => k.StartsWith(prefix)).OrderBy(k => k).ToArray();
    }

    [Fact]
    public void SecureStorageAdapter() =>
        Assert.Equal("سلام", Encoding.UTF8.GetString(AunCoreMethods.SelftestSecureStorage(new MemoryStorage())));

    private sealed class Offline : Connectivity
    {
        public NetworkState Current() => new(false, true);
    }

    [Fact]
    public void ConnectivityAdapter() =>
        Assert.Equal(new NetworkState(false, true), AunCoreMethods.SelftestConnectivity(new Offline()));

    [Fact]
    public async Task ApiClientSurface()
    {
        Assert.Contains(AunCoreMethods.ApiOperations(), o => o.Path == "/api/videos/upload/initiate");
        var client = new ApiClient(new ApiClientConfig("http://127.0.0.1:9", "smoke", "ar", 2000, 1), null);
        var call = new ApiCall("get_GetI18nConfigEndpoint", new Dictionary<string, string>(), new Dictionary<string, string>(), new Dictionary<string, string>(), null, null);
        var e = await Assert.ThrowsAnyAsync<AunException>(() => client.Request(call));
        Assert.True(e is AunException.Network || e is AunException.InvalidInput, e.ToString());
    }

    [Fact]
    public async Task AuthManagerSurface()
    {
        var auth = new AuthManager(
            new AuthConfig("https://auth.example.invalid", "viewer-mobile", "alummahnow://auth/callback", new[] { "openid" }, null, null, null),
            new MemoryStorage());
        Assert.Contains("code_challenge_method=S256", auth.BeginLogin().Url);
        Assert.Empty(auth.Accounts());
        Assert.Null(await auth.AccessToken());
        await Assert.ThrowsAsync<AunException.Unsupported>(() => auth.BeginDeviceLogin());
    }
}
