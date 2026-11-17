// The launch video's stage (launch.html, dev only); scripts/launch.ts films it.
import { createRoot } from "react-dom/client";
import "@t3boat/desktop/brand.css";
import "../site.css";
import "./launch.css";
import { Launch } from "./Launch";

createRoot(document.getElementById("root")!).render(<Launch />);
