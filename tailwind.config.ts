import type { Config } from "tailwindcss";

const config: Config = {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        panel: {
          900: "#07070d",
          800: "#0f1020",
          700: "#171833",
          600: "#21234a"
        },
        accent: {
          400: "#4bffd2",
          500: "#2ff5bf",
          600: "#0ece99"
        },
        ember: {
          500: "#ff9d4d"
        },
        neon: {
          pink: "#ff4fbc",
          cyan: "#44d6ff",
          lime: "#7cff47",
          violet: "#8e6cff"
        }
      },
      boxShadow: {
        glow: "0 0 0 2px rgba(47, 245, 191, 0.45), 0 0 28px rgba(68, 214, 255, 0.35)",
        pixel: "0 0 0 2px #141730, 0 0 0 4px #3e426f, 0 10px 0 #090a14"
      },
      backgroundImage: {
        "hero-glow":
          "radial-gradient(75% 65% at 50% 0%, rgba(68, 214, 255, 0.22) 0%, rgba(7, 7, 13, 0) 62%), radial-gradient(70% 85% at 0% 100%, rgba(255, 79, 188, 0.17) 0%, rgba(7, 7, 13, 0) 58%), linear-gradient(180deg, #0a0a14 0%, #05050b 60%, #04040a 100%)"
      },
      fontFamily: {
        display: ["Press Start 2P", "monospace"],
        body: ["VT323", "monospace"]
      },
      animation: {
        "fade-in": "fadeIn 400ms ease-out",
        "slide-up": "slideUp 300ms ease-out",
        flicker: "flicker 2.5s steps(2) infinite"
      },
      keyframes: {
        fadeIn: {
          from: { opacity: 0 },
          to: { opacity: 1 }
        },
        slideUp: {
          from: { opacity: 0, transform: "translateY(8px)" },
          to: { opacity: 1, transform: "translateY(0px)" }
        },
        flicker: {
          "0%, 19%, 21%, 23%, 80%, 100%": { opacity: "1" },
          "20%, 22%, 81%": { opacity: "0.92" }
        }
      }
    }
  },
  plugins: []
};

export default config;
