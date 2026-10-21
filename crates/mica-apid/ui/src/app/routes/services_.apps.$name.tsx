import { createFileRoute } from '@tanstack/react-router'
import { AppDetailPage } from '@/features/services/app-detail'

export const Route = createFileRoute('/services_/apps/$name')({ component: AppDetailPage })
