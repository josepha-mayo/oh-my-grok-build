import type { CapacitorConfig } from "@capacitor/cli";

const config: CapacitorConfig = {
  appId: "build.omgb.mobile",
  appName: "OMGB Mobile",
  webDir: "dist",
  server: {
    androidScheme: "https",
    iosScheme: "capacitor",
    cleartext: true,
  },
  plugins: {
    SplashScreen: {
      launchShowDuration: 0,
    },
  },
};

export default config;
