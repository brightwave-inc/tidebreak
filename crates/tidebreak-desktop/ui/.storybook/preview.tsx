import type { Decorator, Preview } from "@storybook/react-vite";
import "katex/dist/katex.min.css";
import "../src/styles.css";

const withTidebreakSurface: Decorator = (Story, context) => {
  const dark = context.globals.theme === "dark";
  const fullscreen = context.parameters.layout === "fullscreen";
  document.documentElement.classList.toggle("dark", dark);

  return (
    <div
      className={
        fullscreen
          ? "h-screen min-h-0 overflow-hidden bg-page-background text-foreground"
          : "min-h-screen bg-page-background p-6 text-foreground"
      }
    >
      <Story />
    </div>
  );
};

const preview: Preview = {
  decorators: [withTidebreakSurface],
  globalTypes: {
    theme: {
      description: "Tidebreak color theme",
      toolbar: {
        icon: "paintbrush",
        items: [
          { value: "light", title: "Light", icon: "sun" },
          { value: "dark", title: "Dark", icon: "moon" },
        ],
        dynamicTitle: true,
      },
    },
  },
  initialGlobals: {
    theme: "light",
  },
  parameters: {
    controls: {
      expanded: true,
      sort: "requiredFirst",
    },
    options: {
      storySort: {
        order: [
          "Foundations",
          "Shell",
          "Navigation",
          "Conversation",
          "Composer",
          "Chat",
          "Code",
          "Outputs",
          "Documents",
          "Sources",
          "Attachments",
          "Apps",
          "Plugins",
          "Settings",
          "Modes",
        ],
      },
    },
    viewport: {
      options: {
        desktop: {
          name: "Desktop canvas",
          styles: { width: "1280px", height: "800px" },
        },
        wideDesktop: {
          name: "Wide desktop",
          styles: { width: "1440px", height: "900px" },
        },
        conversation: {
          name: "Conversation pane",
          styles: { width: "760px", height: "800px" },
        },
        minimumWindow: {
          name: "Minimum window (720 × 480)",
          styles: { width: "720px", height: "480px" },
        },
        compact: {
          name: "Compact pane",
          styles: { width: "420px", height: "760px" },
        },
      },
    },
    a11y: {
      test: "todo",
    },
  },
};

export default preview;
