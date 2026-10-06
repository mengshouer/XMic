import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { translations } from "../i18n";
import { LICENSE_NOTICE_DEFINITIONS } from "../licenseNotices";
import { AboutView } from "./AboutView";

function translationShape(value: unknown): unknown {
  if (typeof value !== "object" || value === null) return typeof value;
  return Object.fromEntries(
    Object.entries(value)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, child]) => [key, translationShape(child)]),
  );
}

describe("AboutView", () => {
  it("renders every notice choice without rendering a notice document at startup", () => {
    const markup = renderToStaticMarkup(
      <AboutView
        active={false}
        version="0.3.5"
        copy={translations.en.about}
      />,
    );

    expect(markup.match(/class="about-notice-button"/g)).toHaveLength(
      LICENSE_NOTICE_DEFINITIONS.length,
    );
    expect(markup).not.toContain("<dialog");
    expect(markup).not.toContain("Permission is hereby granted");
  });

  it("keeps English and Chinese About copy structurally identical", () => {
    expect(translationShape(translations.en.about)).toEqual(
      translationShape(translations["zh-CN"].about),
    );

    for (const { id } of LICENSE_NOTICE_DEFINITIONS) {
      expect(translations.en.about.notices[id].title).not.toBe("");
      expect(translations["zh-CN"].about.notices[id].title).not.toBe("");
    }
  });
});
