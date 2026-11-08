pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "aun-core-kotlin"
include(":jvm-smoke")
// The Android library needs the Android SDK; CI jobs without it set AUN_SKIP_ANDROID=1.
if (System.getenv("AUN_SKIP_ANDROID") == null) include(":aun-core")
