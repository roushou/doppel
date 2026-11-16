import { mount } from "svelte";
import App from "./App.svelte";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "./app.css";

const target = document.getElementById("app");
if (target === null) throw new Error("no #app element");
export default mount(App, { target });
