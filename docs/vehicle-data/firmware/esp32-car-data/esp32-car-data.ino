/*
 * LIVI Vehicle Data Box - reference firmware for a car with no OBD/CAN.
 *
 * Reads sensors and pushes TelemetryPayload fields to LIVI over Socket.IO
 * (port 4000, event "telemetry:push").
 *
 * Wiring + calibration: see ../../README.md and the firmware README.
 *
 * Required Arduino libraries:
 *   - ArduinoJson                      (bblanchon)
 *   - Socket.IO client for ESP32       (aion-lp/arduinoSocketIOClient)
 *         depends on links2004/WebSockets
 *
 * Digital inputs arrive via PC817 optocouplers, ACTIVE-LOW.
 */

#include <ArduinoJson.h>
#include <SocketIOClient.h>
#include <WiFi.h>

// -------------------------------------------------------------------------
// Network
// -------------------------------------------------------------------------
const char *WIFI_SSID = "LIVI-xxxx";  // LIVI hotspot, or any LAN the HU is on
const char *WIFI_PASS = "password";
const char *LIVI_HOST  = "192.168.1.100";  // head-unit / LIVI host IP
const uint16_t LIVI_PORT = 4000;

SocketIOClient socketio;

// -------------------------------------------------------------------------
// Pins
// -------------------------------------------------------------------------
// Analog (input-only pins 34/35/36/39 - no pull-ups, fine for dividers)
#define PIN_A_COOLANT 34
#define PIN_A_OIL     35
#define PIN_A_BATTERY 36
#define PIN_A_FUEL    39

// Digital - PC817 outputs, active LOW (10 kohm pull-up to 3V3 on each)
#define PIN_L_TURN    25
#define PIN_R_TURN    26
#define PIN_LOW_BEAM  27
#define PIN_HIGH_BEAM 14
#define PIN_PARK_BRK  12
#define PIN_HAZARDS   33  // set to -1 if unused

// Pulse inputs (also via opto)
#define PIN_RPM  5
#define PIN_SPEED 17  // reed/hall on the speedometer cable; set to -1 if using GPS

// -------------------------------------------------------------------------
// Calibration
// -------------------------------------------------------------------------
#define ADC_PULL_NTC_KOHM   10.0f   // NTC series pull-up to 3V3
#define NTC_R0_OHM          10000.0f
#define NTC_T0_K            298.15f // 25 degC
#define NTC_BETA            3950.0f

#define BATTERY_TOP_KOHM    100.0f  // divider 100k (12V) / 10k (GND)
#define BATTERY_LOW_KOHM    10.0f

#define FUEL_SER_KOHM       0.47f   // series resistor (kohm) to 3V3
#define FUEL_EMPTY_OHM      1.0f    // sender resistance at empty
#define FUEL_FULL_OHM       90.0f   // sender resistance at full

#define PULSES_PER_REV      2       // sparks per crank revolution (4-cyl distributor)
#define PULSES_PER_KM       5000    // speedo-cable pickup pulses per km (calibrate!)

#define TELEMETRY_MS        100     // push rate (10 Hz)

// Optional: infer gear from speed/rpm. 0 = off, 1 = auto table.
#define USE_GEAR_AUTO       0

// -------------------------------------------------------------------------
// State
// -------------------------------------------------------------------------
static volatile uint32_t vol_tachoCount = 0;
static volatile uint32_t vol_wheelCount = 0;
static uint32_t lastTachoCount = 0;
static uint32_t lastWheelCount = 0;
static uint32_t lastPoll = 0;

// Pin helpers
static int8_t pinHazards = PIN_HAZARDS;

void IRAM_ATTR onTachoPulse()  { vol_tachoCount++; }
void IRAM_ATTR onWheelPulse()  { vol_wheelCount++; }

float readV(int pin) {
  return analogRead(pin) / 4095.0f * 3.3f;
}

// 10k NTC + series pull-up to 3V3 -> degC via B-equation.
float ntcToC(int pin) {
  const float r = ADC_PULL_NTC_KOHM * 1000.0f * (readV(pin) / (3.3f - readV(pin)));
  const float k = 1.0f / (1.0f / NTC_T0_K + logf(r / NTC_R0_OHM) / NTC_BETA);
  return k - 273.15f;
}

uint8_t fuelPct(int pin) {
  const float v = readV(pin);
  if (v < 0.01f) return 0;
  const float r = FUEL_SER_KOHM * 1000.0f * (3.3f - v) / v;
  const float span = FUEL_FULL_OHM - FUEL_EMPTY_OHM;
  if (span <= 0) return 0;
  float pct = (r - FUEL_EMPTY_OHM) / span * 100.0f;
  if (pct < 0) pct = 0;
  if (pct > 100) pct = 100;
  return (uint8_t)pct;
}

// Optional gear inference - see gearAutodetect below
const char *gearAutodetect(float speedKph, float rpm) {
  // Crude 3-speed-manual approximation. Validate against your car; the only
  // reliable method is a gear position switch (tie one into PIN_L_TURN unused,
  // or add an extra PC817 input).
  if (rpm < 20 && speedKph < 2) return "N";
  const float ratio = speedKph / fmaxf(rpm, 500.0f);
  if (ratio < 0.010f) return "1";
  if (ratio < 0.018f) return "2";
  return "3";
}

void wifiConnect() {
  WiFi.mode(WIFI_STA);
  WiFi.begin(WIFI_SSID, WIFI_PASS);
  Serial.printf("[net] connecting to %s", WIFI_SSID);
  while (WiFi.status() != WL_CONNECTED) {
    delay(500);
    Serial.print(".");
  }
  Serial.printf("\n[net] connected, IP %s\n", WiFi.localIP().toString().c_str());
}

void socketConnect() {
  if (socketio.connected()) return;
  socketio.begin(LIVI_HOST, LIVI_PORT);
  Serial.printf("[io] connecting to %s:%u\n", LIVI_HOST, LIVI_PORT);
}

void emitTelemetry() {
  StaticJsonDocument<512> doc;

  // Analog every cycle
  doc["coolantC"] = ntcToC(PIN_A_COOLANT);
  doc["oilC"]     = ntcToC(PIN_A_OIL);
  doc["batteryV"] = readV(PIN_A_BATTERY) * ((BATTERY_TOP_KOHM + BATTERY_LOW_KOHM) / BATTERY_LOW_KOHM);
  doc["fuelPct"]  = fuelPct(PIN_A_FUEL);

  // RPM / speed from edge counters (edge-based, simple)
  const uint32_t dt = (millis() - lastPoll);  // ms since last tick
  const uint32_t tachoDelta = vol_tachoCount - lastTachoCount;
  const uint32_t wheelDelta = vol_wheelCount - lastWheelCount;
  lastTachoCount = vol_tachoCount;
  lastWheelCount = vol_wheelCount;

  const float hz = dt > 0 ? (float)tachoDelta * 1000.0f / (float)dt : 0.0f;
  const float rpm = hz * 60.0f / PULSES_PER_REV;

  const float wheelHz = dt > 0 ? (float)wheelDelta * 1000.0f / (float)dt : 0.0f;
  const float speedKph = wheelHz * 3600.0f / PULSES_PER_KM;

  doc["speedKph"] = speedKph;
  doc["rpm"]      = rpm;
  if (USE_GEAR_AUTO) doc["gear"] = gearAutodetect(speedKph, rpm);

  // Digital inputs (active-low opto outputs). Blinkers are reported as-is;
  // hazards OR the two together if the switch is un-wired.
  const bool lt = digitalRead(PIN_L_TURN) == LOW;
  const bool rt = digitalRead(PIN_R_TURN) == LOW;
  doc["lights"]      = digitalRead(PIN_LOW_BEAM) == LOW;
  doc["highBeam"]    = digitalRead(PIN_HIGH_BEAM) == LOW;
  doc["parkingBrake"] = digitalRead(PIN_PARK_BRK) == LOW;

  const bool hazards = pinHazards >= 0 ? digitalRead((uint8_t)pinHazards) == LOW : (lt && rt);
  doc["hazards"] = hazards;
  if (hazards) {
    doc["turn"] = "none";
  } else if (lt) {
    doc["turn"] = "left";
  } else if (rt) {
    doc["turn"] = "right";
  } else {
    doc["turn"] = "none";
  }

  char buf[768];
  serializeJson(doc, buf, sizeof(buf));
  socketio.emit("telemetry:push", buf);
}

void setup() {
  Serial.begin(115200);

  pinMode(PIN_L_TURN, INPUT_PULLUP);
  pinMode(PIN_R_TURN, INPUT_PULLUP);
  pinMode(PIN_LOW_BEAM, INPUT_PULLUP);
  pinMode(PIN_HIGH_BEAM, INPUT_PULLUP);
  pinMode(PIN_PARK_BRK, INPUT_PULLUP);
  if (pinHazards >= 0) pinMode((uint8_t)pinHazards, INPUT_PULLUP);

  analogSetPinAttenuation(PIN_A_COOLANT, ADC_11db);
  analogSetPinAttenuation(PIN_A_OIL, ADC_11db);
  analogSetPinAttenuation(PIN_A_BATTERY, ADC_11db);
  analogSetPinAttenuation(PIN_A_FUEL, ADC_11db);

  if (PIN_RPM >= 0) attachInterrupt(digitalPinToInterrupt(PIN_RPM), onTachoPulse, RISING);
  if (PIN_SPEED >= 0) attachInterrupt(digitalPinToInterrupt(PIN_SPEED), onWheelPulse, RISING);

  wifiConnect();
  socketConnect();
  lastPoll = millis();
}

void loop() {
  socketio.loop();

  const uint32_t now = millis();
  if (now - lastPoll >= TELEMETRY_MS) {
    emitTelemetry();
    lastPoll = now;
  }

  static uint32_t lastReconnect = 0;
  if (millis() - lastReconnect > 5000 && !socketio.connected()) {
    lastReconnect = millis();
    socketConnect();
  }
}