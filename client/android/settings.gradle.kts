// The Cranpose Gradle plugin has no Maven coordinate: it lives inside the `cranpose` crate's own
// source and runs as a composite build, included from wherever cargo resolved that crate. Here
// that is the git checkout of Cranpose at the revision that client/Cargo.toml names. This block
// is the same in every Cranpose Android application.
pluginManagement {
    val cranposePackage = (groovy.json.JsonSlurper().parseText(
        providers.exec { commandLine("cargo", "metadata", "--format-version=1") }
            .standardOutput.asText.get()
    ) as Map<*, *>)["packages"].let { it as List<*> }
        .map { it as Map<*, *> }
        .firstOrNull { it["name"] == "cranpose" }
        ?: error("cargo metadata reports no `cranpose` package; add it as a dependency first")
    val cranposeDir = java.io.File(cranposePackage["manifest_path"] as String).parentFile
    includeBuild(cranposeDir.resolve("android/cranpose-gradle-plugin"))

    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        id("com.android.application") version "9.5.0"
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "cranvr client"
include(":app")
