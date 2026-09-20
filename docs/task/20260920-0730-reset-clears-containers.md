# 20260920-0730-reset-clears-containers The application-data reset clears declared containers

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 07:30

## Description

`reset.rs` documents the `application-data` tier as clearing "operator
applications and their data". Since 20260920-0640 a declared container **is** an
operator application: `container.units` holds it, and the reconciler renders a
Quadlet unit from it.

The tier does not clear that map today. A device reset to application-data
keeps every declaration, so the reconciler renders them again and systemd
starts them: a factory-reset device still running someone's workload, which is
the outcome the tier exists to prevent.

## What changed

`apply` clears `container.units` on the `application-data` tier, beside the
data it already clears. `full-factory` needed no clause: it rebuilds the tree
from the defaults, which declare none.

The switch and everything else in the tree survive: `container.enabled` is
configuration, and configuration is tier 1's business.

## ActiveForm

Clearing declared containers with the applications they are

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

- complete: tier 2 clears the declarations; 545 micad tests green
