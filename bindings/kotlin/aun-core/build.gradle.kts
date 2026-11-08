// Android library (AAR): generated UniFFI Kotlin + libaun_core.so per ABI (from cargo-ndk).
plugins {
    id("com.android.library")
    `maven-publish`
}

android {
    namespace = "net.alummahnow.core"
    compileSdk = 36
    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }
    sourceSets["main"].kotlin.directories.add("../generated")
    sourceSets["main"].jniLibs.directories.add("src/main/jniLibs")
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    publishing {
        singleVariant("release") { withSourcesJar() }
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
}

publishing {
    publications {
        register<MavenPublication>("release") {
            artifactId = "aun-core"
            afterEvaluate { from(components["release"]) }
            pom {
                name.set("aun-core")
                description.set("Al Ummah Now shared Rust core: Kotlin bindings")
                url.set("https://github.com/Al-Ummah-Now/AlUmmahNowCore")
            }
        }
    }
    repositories {
        maven {
            name = "GitHubPackages"
            url = uri("https://maven.pkg.github.com/Al-Ummah-Now/AlUmmahNowCore")
            credentials {
                username = System.getenv("GITHUB_ACTOR")
                password = System.getenv("GITHUB_TOKEN")
            }
        }
    }
}
