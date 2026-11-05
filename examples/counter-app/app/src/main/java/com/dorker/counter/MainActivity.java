package com.dorker.counter;

import android.app.Activity;
import android.content.SharedPreferences;
import android.os.Bundle;
import android.widget.TextView;

/** A counter whose value survives process death. */
public class MainActivity extends Activity {
    static final String PREFS = "counter";
    static final String KEY = "count";

    private SharedPreferences prefs;
    private TextView countView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        countView = findViewById(R.id.count);
        findViewById(R.id.increment).setOnClickListener(v -> set(prefs.getInt(KEY, 0) + 1));
        findViewById(R.id.reset).setOnClickListener(v -> set(0));
        render();
    }

    private void set(int value) {
        prefs.edit().putInt(KEY, value).apply();
        render();
    }

    private void render() {
        countView.setText(String.valueOf(prefs.getInt(KEY, 0)));
    }
}
