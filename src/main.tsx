import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installBrowserContextMenuGuard } from "./browserContextMenu";
import "./styles.css";

installBrowserContextMenuGuard(document);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
