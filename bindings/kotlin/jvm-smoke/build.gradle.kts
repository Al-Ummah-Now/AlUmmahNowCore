// JVM smoke test: loads the host build of libaun_core through JNA and exercises the bindings.
plugins {
    id("org.jetbrains.kotlin.jvm")
}

kotlin { jvmToolchain(17) }

sourceSets["main"].kotlin.srcDir("../generated")

dependencies {
    implementation("net.java.dev.jna:jna:5.19.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
    testImplementation(kotlin("test"))
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.11.0")
}

tasks.test {
    useJUnitPlatform()
    val libDir = rootDir.resolve("../../target/${System.getenv("AUN_PROFILE") ?: "debug"}").absolutePath
    systemProperty("jna.library.path", libDir)
    testLogging { events("passed", "failed"); exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL }
}
