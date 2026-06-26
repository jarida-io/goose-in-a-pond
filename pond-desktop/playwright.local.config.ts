import baseConfig from "./playwright.config";

export default {
  ...baseConfig,
  projects: [
    {
      name: "chromium-local",
      use: {
        ...(baseConfig as { use?: object }).use,
        channel: "chrome",
        video: "off",
        screenshot: "off",
        launchOptions: {
          args: [
            "--autoplay-policy=no-user-gesture-required",
            "--use-fake-ui-for-media-stream",
            "--use-fake-device-for-media-stream",
          ],
        },
      },
    },
  ],
};
