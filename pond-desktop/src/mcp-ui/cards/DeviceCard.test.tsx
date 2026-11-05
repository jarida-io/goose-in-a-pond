import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { DeviceCard } from "./DeviceCard";

describe("the devices card in chat", () => {
  it("does not count a closed dashboard as offline, and says it is not open", () => {
    const { container } = render(
      <DeviceCard
        toolName="giap-device__list_devices"
        variant="normal"
        data={{
          devices: [
            { name: "Pond dashboard", is_online: false, device_type: "desktop" },
            { name: "Smart TV", is_online: false, device_type: "tv" },
            { name: "Emmanuel's A57", is_online: true, device_type: "gotg" },
          ],
        }}
      />,
    );

    expect(screen.getByText("1 offline")).toBeTruthy();
    const titles = Array.from(container.querySelectorAll(".ui-device__dot")).map((d) => d.getAttribute("title"));
    expect(titles).toEqual(["Not open", "Offline", "Online"]);
  });
});
