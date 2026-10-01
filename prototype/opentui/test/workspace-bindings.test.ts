import { expect, test } from "bun:test";
import { keyBindingName, routeForCommand } from "../src/workspace/bindings.js";
test("custom ncspot bindings preserve modifiers and map UI commands locally", () => {
  expect(keyBindingName({ name: "r", shift: true })).toBe("Shift+r");
  expect(keyBindingName({ name: "up", ctrl: true })).toBe("Ctrl+Up");
  expect(keyBindingName({ name: "space" })).toBe("Space");
  expect(routeForCommand("focus queue")).toEqual({ route: "queue" });
  expect(routeForCommand("search ambient jazz")).toEqual({ route: "search", params: { query: "ambient jazz" } });
  expect(routeForCommand("newplaylist Private title")).toBeNull();
});
