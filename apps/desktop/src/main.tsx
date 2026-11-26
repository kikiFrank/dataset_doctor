import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { acknowledgeDesktopRuntime } from "./api/runtime";
import "./style.css";

function App() {
  const [status, setStatus] = useState("正在连接桌面运行时…");

  useEffect(() => {
    void acknowledgeDesktopRuntime().then(
      (version) => setStatus(`桌面运行时已连接 · ${version}`),
      () => setStatus("无法连接桌面运行时"),
    );
  }, []);

  return (
    <div className="app">
      <header><span className="brand-mark" aria-hidden="true">D</span>Dataset Doctor</header>
      <main>
        <h1>工作区</h1>
        <p role="status">{status}</p>
      </main>
    </div>
  );
}

const root = document.getElementById("root");
if (!root) throw new Error("Missing desktop root element");
createRoot(root).render(<App />);
