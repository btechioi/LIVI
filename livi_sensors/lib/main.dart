import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

void main() => runApp(const LiviSensorsApp());

class LiviSensorsApp extends StatelessWidget {
  const LiviSensorsApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'LIVI Sensors',
      theme: ThemeData.dark(useMaterial3: true),
      home: const SensorScreen(),
    );
  }
}

class SensorScreen extends StatefulWidget {
  const SensorScreen({super.key});

  @override
  State<SensorScreen> createState() => _SensorScreenState();
}

class _SensorScreenState extends State<SensorScreen> {
  static const MethodChannel _channel =
      MethodChannel('dev.fio.livi.livisensors/control');

  bool _running = false;
  bool _streaming = false;
  double _heading = 0;
  double _forward = 0;
  double _lateral = 0;
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _refresh();
    _timer = Timer.periodic(const Duration(seconds: 1), (_) => _refresh());
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _refresh() async {
    try {
      final Map<Object?, Object?> s =
          await _channel.invokeMethod('status') as Map<Object?, Object?>;
      setState(() {
        _running = s['running'] as bool? ?? false;
        _streaming = s['streaming'] as bool? ?? false;
        _heading = (s['heading'] as num? ?? 0).toDouble();
        _forward = (s['forward'] as num? ?? 0).toDouble();
        _lateral = (s['lateral'] as num? ?? 0).toDouble();
      });
    } on PlatformException {
      // engine not ready yet; try again on next tick
    }
  }

  Future<void> _start() async {
    await _channel.invokeMethod('start');
    _refresh();
  }

  Future<void> _stop() async {
    await _channel.invokeMethod('stop');
    _refresh();
  }

  @override
  Widget build(BuildContext context) {
    final statusColor = _streaming ? Colors.green : (_running ? Colors.amber : Colors.red);
    return Scaffold(
      appBar: AppBar(title: const Text('LIVI Sensors')),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Card(
              child: ListTile(
                leading: Icon(Icons.circle, color: statusColor),
                title: Text(_streaming
                    ? 'Streaming to rig'
                    : _running
                        ? 'Running — waiting for rig (adb reverse)'
                        : 'Service stopped'),
                subtitle: const Text('Over USB only — no Wi-Fi'),
              ),
            ),
            const SizedBox(height: 12),
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  children: [
                    _stat('Heading', '${_heading.toStringAsFixed(1)}°'),
                    const Divider(color: Colors.white12),
                    _stat('Forward accel', '${_forward.toStringAsFixed(2)} g'),
                    const Divider(color: Colors.white12),
                    _stat('Lateral accel', '${_lateral.toStringAsFixed(2)} g'),
                  ],
                ),
              ),
            ),
            const SizedBox(height: 12),
            const Text(
              'Keep this screen open once — the foreground service keeps '
              'streaming while the phone is locked or Android Auto is on screen.',
              style: TextStyle(color: Colors.white54, fontSize: 12),
            ),
            const Spacer(),
            Row(
              children: [
                Expanded(
                  child: FilledButton.icon(
                    onPressed: _running ? null : _start,
                    icon: const Icon(Icons.play_arrow),
                    label: const Text('Start'),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: _running ? _stop : null,
                    icon: const Icon(Icons.stop),
                    label: const Text('Stop'),
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _stat(String label, String value) {
    return Row(
      mainAxisAlignment: MainAxisAlignment.spaceBetween,
      children: [
        Text(label, style: const TextStyle(color: Colors.white54)),
        Text(value, style: const TextStyle(fontSize: 16)),
      ],
    );
  }
}