// v0.8.19 W2 — smoke tests for the primitive layer: variant classes resolve
// and tailwind-merge lets a caller's className override the variant default.

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { Button } from "./button";
import { Badge } from "./badge";

describe("ui primitives", () => {
  it("Button applies variant + size classes and defaults type=button", () => {
    const html = renderToString(
      <Button variant="destructive" size="sm">
        x
      </Button>,
    );
    expect(html).toContain("text-status-error");
    expect(html).toContain("h-7");
    expect(html).toContain('type="button"');
  });

  it("tailwind-merge: a passed className wins over the variant default", () => {
    const html = renderToString(<Button className="bg-accent-500">x</Button>);
    expect(html).toContain("bg-accent-500");
    expect(html).not.toContain("bg-brand-500");
  });

  it("Badge renders its variant", () => {
    const html = renderToString(<Badge variant="running">live</Badge>);
    expect(html).toContain("text-status-running");
  });
});
