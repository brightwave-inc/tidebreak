/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./app/**/*.{ts,tsx}", "./src/**/*.{ts,tsx}"],
  presets: [require("nativewind/preset")],
  theme: {
    extend: {
      colors: {
        background: "var(--background)",
        foreground: "var(--foreground)",
        "page-background": "var(--page-background)",
        border: "var(--border)",
        muted: "var(--muted)",
        "muted-foreground": "var(--muted-foreground)",
        primary: "var(--primary)",
        "primary-foreground": "var(--primary-foreground)",
        critical: "var(--critical)",
        "critical-foreground": "var(--critical-foreground)",
        "critical-background": "var(--critical-background)",
        "critical-border": "var(--critical-border)",
        success: "var(--success)",
        "success-foreground": "var(--success-foreground)",
        "success-background": "var(--success-background)",
        info: "var(--info)",
        "info-foreground": "var(--info-foreground)",
        "info-background": "var(--info-background)",
        warning: "var(--warning)",
        "warning-foreground": "var(--warning-foreground)",
        live: "var(--live)",
      },
    },
  },
  plugins: [],
};
