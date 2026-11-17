// cranvr's client for headsets, built through the Cranpose Gradle plugin. The plugin builds the
// Rust library, packs it with the OpenXR loader and adds Cranpose's activity and the manifest
// entries of the `xr` service. What remains here belongs to this app.
plugins {
    id("com.android.application")
    id("dev.cranpose.android")
}

cranpose {
    // The Cargo workspace is the client/ directory: two levels above this module.
    workspaceRoot.set("../..")
    cargoPackage.set("cranvr-client")
    label.set("cranvr client")
    // `xr` makes the app an immersive headset app. The link uses plain sockets, so the app needs
    // no Cranpose service for them. Its manifest asks for the Internet permission.
    services.add("xr")
}

android {
    namespace = "dev.cranvr.client2"
    compileSdk = 37

    defaultConfig {
        // A different id from the ALVR based client in quest/, so both install side by side
        applicationId = "dev.cranvr.client2"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            // The debug key signs the APK, so later builds install over earlier ones
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
