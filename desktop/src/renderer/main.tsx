import { createRoot } from "react-dom/client";
import { App } from "./App";
import "highlight.js/styles/github-dark.css";
import "./styles.css";

createRoot(document.getElementById("root")!).render(<App />);
