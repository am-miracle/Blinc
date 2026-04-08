plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.blinc.example"
    // compileSdk 35 (Android 15) gives us the platform headers needed
    // for the 16 KB-page-size APK packaging the Pixel 10 Pro / Android
    // 16 enforces. Bumping past 34 also means the AGP zip-aligner
    // automatically pads native libs to 16 KB.
    compileSdk = 35

    defaultConfig {
        applicationId = "com.blinc.example"
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"

        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    kotlinOptions {
        jvmTarget = "1.8"
    }

    // Force jniLibs to be stored uncompressed and 16 KB-aligned within
    // the APK. AGP 8.5+ does this automatically when targetSdk >= 35,
    // but being explicit guards against future regressions and signals
    // intent. `useLegacyPackaging = false` is the default in AGP 8+ but
    // older example projects sometimes flip it on.
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.appcompat:appcompat:1.6.1")
}

tasks.register<Copy>("copyRustLibs") {
    val rustTargetDir = file("../../../../target")
    val jniLibsDir = file("src/main/jniLibs")

    from("$rustTargetDir/aarch64-linux-android/debug") {
        include("libexample.so")
        into("arm64-v8a")
    }

    into(jniLibsDir)
}

tasks.named("preBuild") {
    dependsOn("copyRustLibs")
}
