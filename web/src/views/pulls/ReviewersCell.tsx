import type { PullRequest } from "../../api/types";
import { ReviewAvatar, reviewStateText } from "../../components/PeoplePicker";
import { useT } from "../../i18n";
import { useMemberKinds } from "../../query/hooks";

/** Display-only: the pull request row containing these avatars owns navigation. */
export function ReviewersCell({ pull }: { pull: PullRequest }) {
  const t = useT();
  const kindOf = useMemberKinds();
  const states = pull.review_states;
  const label = t("people.reviewersSummary", {
    names: states.map((state) => `${state.member} (${reviewStateText(state, t)})`).join(", ") || t("people.nobody"),
  });
  return (
    <span role="img" aria-label={label} title={label} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
      {states.length ? (
        <span aria-hidden style={{ display: "inline-flex", gap: 4 }}>
          {states.map((state) => <ReviewAvatar key={state.member} name={state.member} kind={kindOf(state.member)} state={state} size={24} />)}
        </span>
      ) : <span aria-hidden>—</span>}
    </span>
  );
}
