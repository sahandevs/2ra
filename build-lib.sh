#!/bin/bash

cross build --release --target aarch64-linux-android
rm ui/android/app/src/main/jniLibs/arm64-v8a/lib2raproto.so
mv target/aarch64-linux-android/release/lib2raproto.so ui/android/app/src/main/jniLibs/arm64-v8a/lib2raproto.so