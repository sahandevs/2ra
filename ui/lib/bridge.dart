import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';
import 'package:ffi/ffi.dart';

class Log {
  String? message;
  String? level;

  Log({this.message, this.level});
}

final DynamicLibrary lib2ra =
    DynamicLibrary.open('../target/release/lib2raproto.so');

final Pointer<Utf8> Function() lib2raVersion = lib2ra
    .lookup<NativeFunction<Pointer<Utf8> Function()>>('lib2ra_version')
    .asFunction();

final void Function(Pointer<Int> instance) lib2raStartInstance = lib2ra
    .lookup<NativeFunction<Void Function(Pointer<Int>)>>(
        'lib2ra_start_instance')
    .asFunction();

final Pointer<Int> Function(Pointer<Utf8> config) lib2raNewInstance = lib2ra
    .lookup<NativeFunction<Pointer<Int> Function(Pointer<Utf8>)>>(
        'lib2ra_new_instance')
    .asFunction();

final void Function(Pointer<Int> instance) lib2raStopInstance = lib2ra
    .lookup<NativeFunction<Void Function(Pointer<Int>)>>('lib2ra_stop_instance')
    .asFunction();

final void Function(
        Pointer<NativeFunction<Int8 Function(Int64, Pointer<Dart_CObject>)>>,
        int port) lib2raSetDartSendPort =
    lib2ra
        .lookup<
            NativeFunction<
                Void Function(
                    Pointer<
                        NativeFunction<
                            Int8 Function(Int64, Pointer<Dart_CObject>)>>,
                    Int)>>('lib2ra_set_dart_send_port')
        .asFunction();

class Client {
  Client(
      {this.onLog,
      this.onConnected,
      this.onDisconnected,
      this.onRxState,
      this.onTxState}) {
    _init();
  }

  _init() async {
    await Future.delayed(Duration(milliseconds: 100));
    this.onLog!(Log(
        level: "INFO",
        message: "lib2ra version: ${lib2raVersion.call().toDartString()}"));

    final receivePort = ReceivePort();

    lib2raSetDartSendPort.call(
        NativeApi.postCObject, receivePort.sendPort.nativePort);

    await receivePort.listen(((message) {
      List<dynamic> parts = message;
      final log = Log(level: parts[0], message: parts[1]);
      if (log.message?.isNotEmpty ?? false) {
        this.onLog!(log);
      }
    }));
  }

  void Function(Log log)? onLog;
  void Function()? onConnected;
  void Function()? onDisconnected;

  void Function(bool on)? onRxState;
  void Function(bool on)? onTxState;

  String _config = "";

  void setConfig(String config) {
    this._config = config;
  }

  Pointer<Int> _instancePtr = Pointer.fromAddress(0);

  void shutdown() {
    if (_instancePtr.address == 0) {
      this.onLog!(Log(level: "ERROR", message: "Not connected!"));
      return;
    }
    lib2raStopInstance(_instancePtr);
    _instancePtr = Pointer.fromAddress(0);
    this.onDisconnected!();
  }

  void connect() {
    if (_instancePtr.address != 0) {
      this.onLog!(Log(level: "ERROR", message: "Already Connected!"));
      return;
    }

    _instancePtr = lib2raNewInstance(_config.toNativeUtf8());
    if (_instancePtr.address == 0) {
      this.onLog!(Log(level: "ERROR", message: "Failed to create an instance"));
    }
    lib2raStartInstance(_instancePtr);
    this.onConnected!();
  }
}
