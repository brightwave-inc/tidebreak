import { Textarea } from "tidebreak-desktop-ui";

export function PullRequestBody() {
  return (
    <div style={{ maxWidth: 480 }}>
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Pull request description</span>
        <Textarea
          rows={5}
          defaultValue={
            "Registers the retry timer before yielding to the executor, so the mock clock can no longer advance past it.\n\nCloses #2183."
          }
        />
      </label>
    </div>
  );
}

export function Placeholder() {
  return (
    <div style={{ maxWidth: 480 }}>
      <Textarea rows={3} placeholder={"api.github.com\ncrates.io\nOne host per line."} />
    </div>
  );
}

export function Disabled() {
  return (
    <div style={{ maxWidth: 480 }}>
      <Textarea
        rows={3}
        disabled
        defaultValue={"registry.npmjs.org\nstatic.crates.io"}
      />
    </div>
  );
}
