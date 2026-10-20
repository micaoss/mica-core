import { createFileRoute } from '@tanstack/react-router'
import { UnitDetailPage } from '@/features/services/unit-detail'

export const Route = createFileRoute('/services_/units/$name')({ component: UnitDetailPage })
