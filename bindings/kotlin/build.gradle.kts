plugins {
    id("com.android.library") version "9.5.1" apply false
    id("org.jetbrains.kotlin.jvm") version "2.4.21" apply false
}

// Version comes from the workspace Cargo.toml so the AAR always matches the Rust core.
val coreVersion: String = run {
    val cargo = rootDir.resolve("../../Cargo.toml").readText()
    Regex("""(?m)^version\s*=\s*"([^"]+)"""").find(cargo)?.groupValues?.get(1) ?: "0.0.0"
}
allprojects {
    group = "net.alummahnow"
    version = coreVersion
}
