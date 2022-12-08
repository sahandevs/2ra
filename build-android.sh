#!/bin/bash

cross build --target aarch64-linux-android --release

rm -r ui/android/app/src/main/jniLibs 
mkdir -p ui/android/app/src/main/jniLibs/arm64-v8a
cp ./target/aarch64-linux-android/release/lib2raproto.so ui/android/app/src/main/jniLibs/arm64-v8a