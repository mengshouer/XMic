import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyThemeToDocument, readStoredThemePreference } from "./theme";

applyThemeToDocument(readStoredThemePreference());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
