// cranvr's client for headsets, built through the Cranpose Gradle plugin. The plugin builds the
// Rust library, packs it with the OpenXR loader and adds Cranpose's activity and the manifest
// entries of the `xr` service. What remains here belongs to this app.
//
// The same crate builds a second APK for Android phones and tablets. Pass `-PcranvrPhone=true`
// (`scripts/build-client.sh --phone` does). That APK has its own application id and label. It is a
// flat app with no `xr` service and no OpenXR loader. The screen shows the picture and the
// device's motion sensors are the head. The plugin takes one list of Cargo features for a
// project, so a property switches the whole module. A build makes one APK.
plugins {
    id("com.android.application")
    id("dev.cranpose.android")
}

val phone = providers.gradleProperty("cranvrPhone").map { it == "true" }.orElse(false).get()

cranpose {
    // The Cargo workspace is the client/ directory: two levels above this module.
    workspaceRoot.set("../..")
    cargoPackage.set("cranvr-client")
    if (phone) {
        label.set("cranvr phone")
        features.set(listOf("phone", "renderer-wgpu"))
    } else {
        label.set("cranvr client")
        // `xr` makes the app an immersive headset app. The link uses plain sockets, so the app
        // needs no Cranpose service for them. Its manifest asks for the Internet permission.
        services.add("xr")
    }
}

android {
    namespace = "dev.cranvr.client2"
    compileSdk = 37

    defaultConfig {
        // The phone's APK has its own id, so it installs beside the Quest's.
        applicationId = if (phone) "dev.cranvr.client2.phone" else "dev.cranvr.client2"
        minSdk = 24
        targetSdk = 36
        // A store takes an upload only with a higher version code than the last one
        versionCode = providers.gradleProperty("cranvrVersionCode").map { it.toInt() }.orElse(1).get()
        versionName = providers.gradleProperty("cranvrVersionName").orElse("0.1.0").get()
    }

    // A store build signs with the app's own upload key: CRANVR_KEYSTORE names the keystore file,
    // CRANVR_KEYSTORE_PASSWORD and CRANVR_KEY_ALIAS open it (the key's password is the store's).
    // Without them the debug key signs, so later test builds install over earlier ones.
    val keystore = providers.environmentVariable("CRANVR_KEYSTORE").orNull
    if (keystore != null) {
        signingConfigs.create("store") {
            storeFile = file(keystore)
            storePassword = providers.environmentVariable("CRANVR_KEYSTORE_PASSWORD").get()
            keyAlias = providers.environmentVariable("CRANVR_KEY_ALIAS").get()
            keyPassword = storePassword
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            signingConfig = signingConfigs.getByName(if (keystore != null) "store" else "debug")
        }
    }

    // The phone's manifest turns the activity to landscape. The Quest's activity is the xr
    // service's business.
    if (phone) {
        sourceSets.getByName("main").manifest.srcFile("src/phone/AndroidManifest.xml")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
