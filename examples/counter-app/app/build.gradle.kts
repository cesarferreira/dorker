plugins {
    id("com.android.application")
}

android {
    namespace = "com.dorker.counter"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.dorker.counter"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        // ./gradlew -PdorkerDemoFailure ... makes DemoFailureTest fail on purpose.
        buildConfigField(
            "boolean",
            "DEMO_FAILURE",
            (project.findProperty("dorkerDemoFailure") != null).toString()
        )
    }

    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
}
