import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { createQueryClient } from "./query/hooks";
import "@mantine/core/styles.css";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("#root is missing");
createRoot(root).render(
  <StrictMode>
    <App client={createQueryClient()} />
  </StrictMode>,
);
