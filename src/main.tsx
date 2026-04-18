import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./app/App";
import appLogo from "./public/noland.png";
import "./index.css";

const favicon = document.querySelector("link[rel='icon']") || document.createElement("link");
favicon.setAttribute("rel", "icon");
favicon.setAttribute("type", "image/png");
favicon.setAttribute("href", appLogo);
if (!favicon.parentNode) {
  document.head.appendChild(favicon);
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
