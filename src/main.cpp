// Side screen: from-scratch bring-up for the Sunton ESP32-8048S043C.
//
// Step 1: backlight on, then full red, green, blue, white so you can see the panel works.
// Step 2: a simple interface with a title, two buttons and a touch counter.
// The xBloom Bluetooth part comes after this works.
//
// Pins are the ones the board's published examples use (and match the xbloom-screen firmware).
#include <Arduino.h>
#include <Wire.h>
#include <Arduino_GFX_Library.h>
#include <TAMC_GT911.h>

#define GFX_BL 2

#define TOUCH_SDA 19
#define TOUCH_SCL 20
#define TOUCH_INT 18
#define TOUCH_RST 38
#define SCREEN_W 800
#define SCREEN_H 480

Arduino_ESP32RGBPanel *rgbpanel = new Arduino_ESP32RGBPanel(
    GFX_NOT_DEFINED /* CS */, GFX_NOT_DEFINED /* SCK */, GFX_NOT_DEFINED /* SDA */,
    40 /* DE */, 41 /* VSYNC */, 39 /* HSYNC */, 42 /* PCLK */,
    45 /* R0 */, 48 /* R1 */, 47 /* R2 */, 21 /* R3 */, 14 /* R4 */,
    5 /* G0 */, 6 /* G1 */, 7 /* G2 */, 15 /* G3 */, 16 /* G4 */, 4 /* G5 */,
    8 /* B0 */, 3 /* B1 */, 46 /* B2 */, 9 /* B3 */, 1 /* B4 */,
    0 /* hsync_polarity */, 8 /* hsync_front_porch */, 4 /* hsync_pulse_width */, 8 /* hsync_back_porch */,
    0 /* vsync_polarity */, 8 /* vsync_front_porch */, 4 /* vsync_pulse_width */, 8 /* vsync_back_porch */,
    1 /* pclk_active_neg */, 16000000 /* prefer_speed */);

Arduino_RGB_Display *gfx = new Arduino_RGB_Display(SCREEN_W, SCREEN_H, rgbpanel, 0 /* rotation */, true /* auto_flush */);

TAMC_GT911 ts(TOUCH_SDA, TOUCH_SCL, TOUCH_INT, TOUCH_RST, SCREEN_W, SCREEN_H);

static const uint16_t BG = 0x2120;  // dark green
static const uint16_t CREAM = 0xF7BB;
static int taps = 0;

static void drawButton(int x, int y, int w, int h, const char *label, uint16_t fill) {
  gfx->fillRoundRect(x, y, w, h, 16, fill);
  gfx->setTextColor(CREAM);
  gfx->setTextSize(3);
  int16_t tx, ty;
  uint16_t tw, th;
  gfx->getTextBounds(label, 0, 0, &tx, &ty, &tw, &th);
  gfx->setCursor(x + (w - tw) / 2, y + (h - th) / 2);
  gfx->print(label);
}

static void drawCounter() {
  gfx->fillRect(40, 300, 720, 60, BG);
  gfx->setTextColor(CREAM);
  gfx->setTextSize(4);
  gfx->setCursor(40, 310);
  gfx->printf("Touches: %d", taps);
}

static void drawUi() {
  gfx->fillScreen(BG);
  gfx->setTextColor(CREAM);
  gfx->setTextSize(6);
  gfx->setCursor(40, 40);
  gfx->print("side");
  gfx->setTextSize(3);
  gfx->setCursor(40, 120);
  gfx->print("Screen test OK. Touch a button.");
  drawButton(40, 180, 300, 90, "Button A", 0x4B87);
  drawButton(400, 180, 300, 90, "Button B", 0x8A22);
  drawCounter();
}

void setup() {
  Serial.begin(115200);
  delay(300);
  Serial.println("side-screen: start");

  pinMode(GFX_BL, OUTPUT);
  digitalWrite(GFX_BL, HIGH);
  Serial.println("side-screen: backlight on");

  if (!gfx->begin()) {
    Serial.println("side-screen: gfx->begin() FAILED");
  } else {
    Serial.println("side-screen: panel started");
  }

  const struct { const char *name; uint16_t c; } colours[] = {
      {"red", 0xF800}, {"green", 0x07E0}, {"blue", 0x001F}, {"white", 0xFFFF}};
  for (auto &col : colours) {
    gfx->fillScreen(col.c);
    Serial.printf("side-screen: colour %s\n", col.name);
    delay(1200);
  }

  Wire.begin(TOUCH_SDA, TOUCH_SCL);
  ts.begin();
  ts.setRotation(ROTATION_NORMAL);
  Serial.println("side-screen: touch started");

  drawUi();
}

void loop() {
  static bool down = false;
  ts.read();
  if (ts.isTouched) {
    int x = ts.points[0].x, y = ts.points[0].y;
    if (!down) {
      down = true;
      taps++;
      Serial.printf("side-screen: touch %d,%d\n", x, y);
      drawCounter();
    }
    gfx->fillCircle(x, y, 10, CREAM);
  } else {
    down = false;
  }
  delay(10);
}
