# Shipright review status — PR #3074

Reviewed range: e8460fe12756652ddae473ef4fedd4febfecfa7b..39238cfc15162dbdfa1d82c2460e7a801fbfb340.

The review found one confirmed P2 correctness issue in the internal-turn restart reclaim path: the worker can resume a turn without first backfilling its missing input transcript row, leaving that turn with a nil input message id and no user transcript entry.

The review could not be submitted because the governed createPullRequestReview operation is unavailable in this sandbox. No review has been posted on GitHub.
