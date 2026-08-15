import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    environment: 'node',
    restoreMocks: true,
    clearMocks: true,
    include: ['tests/**/*.test.ts'],
  },
})
