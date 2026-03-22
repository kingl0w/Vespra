/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,jsx}"],
  theme: {
    extend: {
      colors: {
        vespra: {
          bg: "#0a0a0a",
          surface: "#141414",
          border: "#262626",
          accent: "#D4A017",
          "accent-dim": "#A37E12",
          "accent-glow": "#F5C518",
          text: "#f0ebe0",
          muted: "#787060",
          green: "#22c55e",
          red: "#ef4444",
          yellow: "#eab308",
        },
      },
      fontFamily: {
        mono: ['"JetBrains Mono"', '"Fira Code"', "monospace"],
      },
    },
  },
  plugins: [],
};
