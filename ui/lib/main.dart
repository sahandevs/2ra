import 'dart:convert';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'dart:ffi'; // For FFI
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:system_info2/system_info2.dart';

import 'bridge.dart'; // For Platform.isX

void main() {
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '2ra UI',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        brightness: Brightness.dark,
        primarySwatch: Colors.orange,
      ),
      home: const MyHomePage(title: '2ra'),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({super.key, required this.title});

  final String title;

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  bool _isConnected = false;
  bool _isRxOn = false;
  bool _isTxOn = false;

  Client? _client;

  List<Log> logs = [];

  _MyHomePageState() {
    this._client = Client(
      onConnected: () {
        setState(() {
          _isConnected = true;
        });
      },
      onDisconnected: () {
        setState(() {
          _isConnected = false;
        });
      },
      onLog: (log) {
        this.logs.add(log);
        setState(() {
          _scrollController.animateTo(
              _scrollController.position.maxScrollExtent,
              duration: Duration(milliseconds: 50),
              curve: Curves.easeInOut);
        });
      },
      onRxState: (isOn) {
        setState(() {
          _isRxOn = true;
        });
      },
      onTxState: (isOn) {
        setState(() {
          _isTxOn = true;
        });
      },
    );

    _init();
  }

  final ScrollController _scrollController = ScrollController();

  TextEditingController _controller = TextEditingController(text: "");
  _init() async {
    final prefs = await SharedPreferences.getInstance();
    final _config = await prefs.getString("config") ?? "";
    _controller.text = _config;
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Padding(
        padding: const EdgeInsets.all(8.0),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          mainAxisSize: MainAxisSize.max,
          children: <Widget>[
            Container(
              height: 30,
            ),
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceAround,
              children: const [
                Text("TX"),
                Text("RX"),
              ],
            ),
            Container(
              height: 10,
            ),
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceAround,
              children: [
                ChannelStateIndicator(_isTxOn ? Colors.green : Colors.red),
                ChannelStateIndicator(_isRxOn ? Colors.green : Colors.red),
              ],
            ),
            Container(
              height: 10,
            ),
            TextFormField(
              minLines: 10,
              keyboardType: TextInputType.multiline,
              maxLines: 20,
              controller: _controller,
              onChanged: (value) async {
                final prefs = await SharedPreferences.getInstance();
                await prefs.setString("config", value);
              },
            ),
            Container(
              height: 10,
            ),
            Expanded(
              child: ListView.builder(
                  itemCount: this.logs.length,
                  controller: _scrollController,
                  itemBuilder: ((context, index) {
                    return buildLogItem(this.logs[index]);
                  })),
            )
          ],
        ),
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: () {
          if (_isConnected) {
            this._client?.shutdown();
          } else {
            this._client?.setConfig(_controller.text);
            this._client?.connect();
          }
        },
        tooltip: _isConnected ? 'Disconnect' : 'Connect',
        backgroundColor: _isConnected ? Colors.green : Colors.grey,
        child: Icon(_isConnected ? Icons.wifi : Icons.wifi_off),
      ),
    );
  }

  Widget buildLogItem(Log log) {
    return Padding(
      padding: const EdgeInsets.all(2.0),
      child: Row(
        children: [
          Container(
            decoration: BoxDecoration(
              color: {
                    "ERROR": Colors.red,
                    "DEBUG": Colors.purple,
                    "INFO": Colors.blue,
                    "WARNING": Colors.yellowAccent
                  }[log.level ?? ""] ??
                  Colors.blue,
              borderRadius: BorderRadius.circular(3),
            ),
            padding: EdgeInsets.all(3),
            child: Text(log.level ?? "?"),
          ),
          Padding(
            padding: const EdgeInsets.only(left: 8.0),
            child: Text(log.message ?? "-"),
          )
        ],
      ),
    );
  }

  Container ChannelStateIndicator(Color color) {
    return Container(
      height: 20,
      width: 20,
      decoration:
          BoxDecoration(borderRadius: BorderRadius.circular(2), color: color),
    );
  }
}
