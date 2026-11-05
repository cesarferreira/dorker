# Dorker Counter

A small real Android app (Java, AGP 9.1.1, Gradle 9.3.1) used to exercise dorker during
development: a counter that persists across process death, with three Espresso tests.

```bash
cd examples/counter-app
# Use a locally built dorker (or just ./dev.sh with dorker on your PATH)
DORKER=../../target/debug/dorker ./dev.sh
```

`dev.sh` builds both APKs with Gradle, builds the image (`Dorkerfile`), starts an instance,
saves `counter-dev.png`, runs the instrumentation tests and removes the instance.
Needs a JDK 17+ and Android SDK platform 35. Manual steps:

```bash
./gradlew assembleDebug assembleDebugAndroidTest
dorker build -t counter:dev .
dorker run -d --name c1 counter:dev           # launches the app (CMD launch)
dorker screenshot c1 shot.png
dorker test c1 \
  --runner com.dorker.counter.test/androidx.test.runner.AndroidJUnitRunner
dorker rm -f c1
```
