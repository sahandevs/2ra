class Log {
  String? message;
  String? level;

  Log({this.message, this.level});
}

class Client {
  Client(
      {this.onLog,
      this.onConnected,
      this.onDisconnected,
      this.onRxState,
      this.onTxState}) {}

  void Function(Log)? onLog;
  void Function()? onConnected;
  void Function()? onDisconnected;

  void Function(bool on)? onRxState;
  void Function(bool on)? onTxState;

  void setConfig(String config) {}

  void shutdown() {}

  void connect() {}
}


  // void _reloadLib() async {
  //   final manifestContent = await rootBundle.loadString('AssetManifest.json');
  //   print(SysInfo.kernelArchitecture);

  //   final Map<String, dynamic> manifestMap = json.decode(manifestContent);

  //   manifestMap.forEach((key, value) {
  //     print("${key}:${value}");
  //   });
  //   final DynamicLibrary nativeAddLib = DynamicLibrary.open('lib2raproto.so');

  //   final int Function() nativeAdd = nativeAddLib
  //       .lookup<NativeFunction<Int32 Function()>>('lib2ra_start_test_server')
  //       .asFunction();
  //   setState(() {
  //     result = nativeAdd();
  //   });
  // }