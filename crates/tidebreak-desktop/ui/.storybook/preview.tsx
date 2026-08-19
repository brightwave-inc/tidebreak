import type { Decorator, Preview } from "@storybook/react-vite";
import "katex/dist/katex.min.css";
import "../src/styles.css";

const withTidebreakSurface: Decorator = (Story, context) => {
  const dark = context.globals.theme === "dark";
  document.documentElement.classList.toggle("dark", dark);

  return (
    <div className="min-h-screen bg-page-background p-6 text-foreground">
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
        order: ["Foundations", "Conversation", "Code"],
      },
    },
    viewport: {
      options: {
        desktop: {
          name: "Desktop canvas",
          styles: { width: "1280px", height: "800px" },
        },
        conversation: {
          name: "Conversation pane",
          styles: { width: "760px", height: "800px" },
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
