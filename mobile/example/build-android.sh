#!/bin/bash
# Build script for Android mobile example
#
# Usage:
#   bash build-android.sh                  # auto-pick first authorized device
#   ANDROID_SERIAL=emulator-5554 bash …    # target a specific device
#   bash build-android.sh -s <serial>      # ditto, via positional arg

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Set Java 17 for Android Gradle Plugin compatibility
export JAVA_HOME=/opt/homebrew/opt/openjdk@17

# Set Android SDK paths
export ANDROID_HOME=~/Library/Android/sdk
ADB="$ANDROID_HOME/platform-tools/adb"

# Pin NDK to r29: NDK r28+ links arm64-v8a .so files with 16 KB ELF
# segment alignment by default, which is required for installs on
# Android 16 / Pixel 10 Pro. r27 still uses 4 KB segments and produces
# APKs that fail with "Uncompressed library not aligned" on those
# devices. cargo-ndk auto-discovers the *highest* NDK under
# `$ANDROID_HOME/ndk/`, but pinning explicitly avoids accidents when
# new NDKs land.
if [ -z "$ANDROID_NDK_HOME" ]; then
    if [ -d "$ANDROID_HOME/ndk/29.0.14206865" ]; then
        export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/29.0.14206865"
    fi
fi
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
export NDK_HOME="$ANDROID_NDK_HOME"

# Allow `-s <serial>` override on the command line
if [ "$1" = "-s" ] && [ -n "$2" ]; then
    export ANDROID_SERIAL="$2"
fi

# Resolve target device:
#   1. ANDROID_SERIAL env var if set (and authorized)
#   2. Otherwise the first `device` (authorized) entry from `adb devices`
#      — unauthorized / offline / no_permissions entries are skipped.
if [ -n "$ANDROID_SERIAL" ]; then
    DEVICE_SERIAL="$ANDROID_SERIAL"
else
    DEVICE_SERIAL=$($ADB devices | awk '$2 == "device" { print $1; exit }')
fi

if [ -z "$DEVICE_SERIAL" ]; then
    echo "No authorized device connected. Plug one in and accept the USB debugging prompt."
    echo "(APK will still be built and left at platforms/android/app/build/outputs/apk/debug/app-debug.apk)"
fi

# Step 1: Build Rust library for Android
echo "Building Rust library for arm64-v8a..."
cargo ndk -t arm64-v8a -o platforms/android/app/src/main/jniLibs build --release

# Step 2: Build APK
echo "Building APK..."
cd platforms/android
./gradlew assembleDebug

if [ -n "$DEVICE_SERIAL" ]; then
    echo "Installing APK to $DEVICE_SERIAL..."
    $ADB -s "$DEVICE_SERIAL" install -r app/build/outputs/apk/debug/app-debug.apk

    echo "Starting app on $DEVICE_SERIAL..."
    $ADB -s "$DEVICE_SERIAL" shell am start -n com.blinc.example/.MainActivity

    echo "Showing logs (Ctrl+C to exit)..."
    $ADB -s "$DEVICE_SERIAL" logcat -c  # Clear old logs
    $ADB -s "$DEVICE_SERIAL" logcat -s Blinc:D RustStdoutStderr:D AndroidRuntime:E DEBUG:F BlincNativeBridge:D
else
    echo "APK is at:"
    echo "  platforms/android/app/build/outputs/apk/debug/app-debug.apk"
fi
