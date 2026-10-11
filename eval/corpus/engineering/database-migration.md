# Database migration rollout
Schema changes should use expand and contract. Add a nullable field first, deploy readers and writers, backfill records in batches, then enforce the constraint after old application versions are gone.
