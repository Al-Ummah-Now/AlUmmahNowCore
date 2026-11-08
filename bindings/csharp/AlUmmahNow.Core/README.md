# AlUmmahNow.Core

C# bindings for the Al Ummah Now shared Rust core. Native `aun_core` libraries ship for
`win-x64`, `win-arm64`, `linux-x64`, `linux-arm64` and `osx` (universal).

```csharp
using AlUmmahNow.Core;
Console.WriteLine(AunCoreMethods.CoreVersion());
```
