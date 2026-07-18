import type { CapacitorConfig } from "@capacitor/cli";

const config: CapacitorConfig = {
  appId: "ai.devin.omgb",
  appName: "Grok Build",
  webDir: "dist",
  server: {
    androidScheme: "capacitor",
    iosScheme: "capacitor",
  },
};

export default config;
