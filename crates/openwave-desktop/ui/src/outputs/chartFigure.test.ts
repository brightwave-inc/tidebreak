import { describe, expect, it } from "vitest";

import { parseChartFigure } from "./chartFigure";

describe("parseChartFigure", () => {
  it("accepts a figure and defaults the layout", () => {
    const figure = parseChartFigure(
      '{"data":[{"type":"bar","x":["a"],"y":[1]}]}',
    );
    expect(figure?.data).toHaveLength(1);
    expect(figure?.layout).toEqual({});
  });

  it("rejects anything that is not a figure", () => {
    expect(parseChartFigure("not json at all")).toBeNull();
    // A bare trace list, the shape a model most plausibly writes by mistake.
    expect(parseChartFigure('[{"type":"bar"}]')).toBeNull();
    expect(parseChartFigure('{"data":[]}')).toBeNull();
    expect(parseChartFigure('{"data":["bar"]}')).toBeNull();
    expect(parseChartFigure('{"layout":{"title":"x"}}')).toBeNull();
  });
});
