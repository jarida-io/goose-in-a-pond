import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { StepsDisclosure } from "./StepsDisclosure";

afterEach(cleanup);

describe("StepsDisclosure", () => {
  it("is collapsed until it is opened", () => {
    render(<StepsDisclosure steps={["Let me check the weather.", "Now the news."]} />);
    const toggle = screen.getByRole("button", { name: "Steps" });

    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Let me check the weather.")).toBeNull();

    fireEvent.click(toggle);

    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Let me check the weather.")).toBeTruthy();
    expect(screen.getByText("Now the news.")).toBeTruthy();
  });
});
