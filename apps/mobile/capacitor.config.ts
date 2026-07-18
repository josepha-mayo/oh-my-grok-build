import type { CapacitorConfig } from "@capacitor/cli";

const config: CapacitorConfig = {
  appId: "build.omgb.mobile",
  appName: "OMGB Mobile",
  webDir: "dist",
  server: {
    androidScheme: "https",
    iosScheme: "capacitor",
    // Cleartext is intentionally enabled for local ws:// pairing with the
    // desktop relay. The app's CSP restricts connect-src to ws/wss, and code
    // rejects non-local ws:// hosts, so this only affects the pairing channel.
    cleartext: true,
  },
  plugins: {
    SplashScreen: {
      launchShowDuration: 0,
    },
  },
};

export default config;
