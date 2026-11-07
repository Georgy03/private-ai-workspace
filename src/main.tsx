import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import App from "./App";
import "./styles.css";
import "./layout.css";
import "./theme.css";
import "./dark.css";
import { startTheme } from "./theme";

// The saved look is on the page before anything is drawn, so there is no flash of the other one.
startTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
