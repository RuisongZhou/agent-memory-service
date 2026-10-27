import { expect, it } from 'vitest'
import { wordDiff } from './diff'

it('marks only the changed words of a correction, in Chinese and English alike', () => {
  expect(wordDiff('服务端口是 8765，部署在测试机上。', '服务端口是 9000，部署在生产机上。')).toEqual([
    { kind: 'same', text: '服务端口是 ' },
    { kind: 'removed', text: '8765' },
    { kind: 'added', text: '9000' },
    { kind: 'same', text: '，部署在' },
    { kind: 'removed', text: '测试' },
    { kind: 'added', text: '生产' },
    { kind: 'same', text: '机上。' },
  ])
  expect(wordDiff('The port is 8765 now.', 'The port is 8765 now.')).toEqual([
    { kind: 'same', text: 'The port is 8765 now.' },
  ])
})
