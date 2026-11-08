# AlUmmahNowCore

Al Ummah Now shared Rust core (`aun-core`, `aun-media`): API client, auth, sync, upload engine and
the Studio media engine (encoding plans, transcription, edit model), with Swift, Kotlin and C#
bindings for the native apps.

- Plan and architecture: [`docs/PLAN.md`](docs/PLAN.md)
- Changes: [`CHANGELOG.md`](CHANGELOG.md)

## Use it

**Swift (iOS, iPadOS, tvOS, macOS)**

```swift
.package(url: "https://github.com/Al-Ummah-Now/AlUmmahNowCore", exact: "0.2.0")
// target dependency: .product(name: "AunCore", package: "AlUmmahNowCore")
import AunCore
print(coreVersion())
```

**Kotlin (Android)** — GitHub Packages Maven (needs a token with `read:packages`):

```kotlin
repositories {
    maven("https://maven.pkg.github.com/Al-Ummah-Now/AlUmmahNowCore") {
        credentials { username = gprUser; password = gprToken }
    }
}
dependencies { implementation("net.alummahnow:aun-core:0.2.0") }
// import net.alummahnow.core.*; coreVersion()
```

**C# (.NET 8+: Windows, Linux, macOS)** — GitHub Packages NuGet:

```sh
dotnet nuget add source https://nuget.pkg.github.com/Al-Ummah-Now/index.json -n aun -u <user> -p <token>
dotnet add package AlUmmahNow.Core --version 0.2.0
```

```csharp
using AlUmmahNow.Core;
Console.WriteLine(AunCoreMethods.CoreVersion());
```

## Develop

See [`CLAUDE.md`](CLAUDE.md) for commands. Releases happen only by pushing a `vX.Y.Z` tag on `main`; see [`docs/RELEASING.md`](docs/RELEASING.md).
