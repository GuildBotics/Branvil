import type { PullRequest } from "../../api/types";
import { ReviewAvatar, reviewStates, reviewStateText } from "../../components/PeoplePicker";
import { useT } from "../../i18n";
import { useMemberKinds, usePullActivity } from "../../query/hooks";

/** Display-only: the pull request row containing these avatars owns navigation. */
export function ReviewersCell({ pull }: { pull: PullRequest }) {
  const t = useT();
  const activity = usePullActivity(pull.repository, pull.number);
  const kindOf = useMemberKinds();
  const states = activity.isSuccess ? [...reviewStates(pull, activity.data.reviews)] : [];
  const label = activity.isError ? t("people.reviewersUnavailable") : activity.isPending ? t("common.loading") : t("people.reviewersSummary", {
    names: states.map(([name, state]) => `${name} (${reviewStateText(state, t)})`).join(", ") || t("people.nobody"),
  });
  return (
    <span role="img" aria-label={label} title={label} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
      {states.length ? (
        <span aria-hidden style={{ display: "inline-flex", gap: 4 }}>
          {states.map(([name, state]) => <ReviewAvatar key={name} name={name} kind={kindOf(name)} state={state} size={24} />)}
        </span>
      ) : <span aria-hidden>—</span>}
    </span>
  );
}
