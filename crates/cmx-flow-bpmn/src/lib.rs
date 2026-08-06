/*
 * @Describe: cmx-flow-bpmn —— BPMN 2.0 XML 编译器。
 *
 * 对外仅暴露 `compile(xml) -> ProcessDefinition`。BPMN 作为交换格式，编译成中立 IR 后
 * 引擎不再接触 XML。M1 支持子集：startEvent / endEvent / userTask / serviceTask /
 * exclusiveGateway + 带条件 sequenceFlow；遇到未支持的活动类元素显式报错而非静默丢弃。
 */

pub mod compiler;
pub mod error;

pub use compiler::compile;
pub use error::{Error, Result};

#[cfg(test)]
mod tests {
    use super::*;
    use cmx_flow_model::NodeKind;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             targetNamespace="http://cmx/flow">
  <process id="leave_request" name="请假审批" isExecutable="true">
    <startEvent id="start" name="发起"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="经理审批" flowable:assignee="manager"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="gw"/>
    <exclusiveGateway id="gw" name="金额判断" default="f_small"/>
    <sequenceFlow id="f_big" sourceRef="gw" targetRef="director">
      <conditionExpression xsi:type="tFormalExpression"
           xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">${amount &gt; 1000}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f_small" sourceRef="gw" targetRef="done"/>
    <userTask id="director" name="总监审批" flowable:assignee="director"/>
    <sequenceFlow id="f3" sourceRef="director" targetRef="done"/>
    <endEvent id="done" name="结束"/>
  </process>
</definitions>"#;

    #[test]
    fn compiles_sample_process() {
        let def = compile(SAMPLE).expect("应能编译样例流程");
        assert_eq!(def.key, "leave_request");
        assert_eq!(def.name.as_deref(), Some("请假审批"));
        // 6 个流程节点：start, review, gw, director, done + ... = 5 实际
        assert_eq!(def.nodes.len(), 5);

        // start 指向 startEvent
        assert!(matches!(def.node(def.start).kind, NodeKind::StartEvent));

        // userTask review 带 assignee
        let review = def.node_by_bpmn("review").unwrap();
        match &review.kind {
            NodeKind::UserTask(ut) => assert_eq!(ut.assignee.as_deref(), Some("manager")),
            other => panic!("review 应为 userTask，实际 {other:?}"),
        }

        // 网关有两条出边，f_small 为 default，f_big 带条件
        let gw = def.node_by_bpmn("gw").unwrap();
        assert_eq!(gw.outgoing.len(), 2);
        let big = gw.outgoing.iter().find(|f| f.bpmn_id == "f_big").unwrap();
        let small = gw.outgoing.iter().find(|f| f.bpmn_id == "f_small").unwrap();
        assert!(big.condition.is_some(), "f_big 应带条件");
        assert!(big.condition.as_deref().unwrap().contains("amount"));
        assert!(small.is_default, "f_small 应为 default");
    }

    #[test]
    fn rejects_missing_start() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="p" isExecutable="true">
            <endEvent id="e"/>
          </process></definitions>"#;
        let err = compile(xml).unwrap_err();
        assert!(matches!(err, Error::MissingElement(_)));
    }

    #[test]
    fn rejects_unsupported_element() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f" sourceRef="s" targetRef="sp"/>
            <subProcess id="sp"/>
          </process></definitions>"#;
        let err = compile(xml).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "子流程 M2 仍应报不支持"
        );
    }

    #[test]
    fn compiles_parallel_gateway_with_incoming_counts() {
        // fork/join：s → fork → (a,b) → join → e
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="fork"/>
            <parallelGateway id="fork"/>
            <sequenceFlow id="f1" sourceRef="fork" targetRef="a"/>
            <sequenceFlow id="f2" sourceRef="fork" targetRef="b"/>
            <userTask id="a" flowable:assignee="ua"/>
            <userTask id="b" flowable:assignee="ub"/>
            <sequenceFlow id="f3" sourceRef="a" targetRef="join"/>
            <sequenceFlow id="f4" sourceRef="b" targetRef="join"/>
            <parallelGateway id="join"/>
            <sequenceFlow id="f5" sourceRef="join" targetRef="e"/>
            <endEvent id="e"/>
          </process></definitions>"#;
        let def = compile(xml).expect("并行网关应能编译");
        let fork = def.node_by_bpmn("fork").unwrap();
        assert!(matches!(fork.kind, NodeKind::ParallelGateway));
        assert_eq!(fork.outgoing.len(), 2, "fork 有 2 条出边");
        assert_eq!(fork.incoming_count, 1, "fork 入边 1");
        let join = def.node_by_bpmn("join").unwrap();
        assert_eq!(join.incoming_count, 2, "join 入边 2（等 2 个令牌）");
        assert_eq!(join.outgoing.len(), 1, "join 出边 1");
    }

    #[test]
    fn compiles_parallel_multi_instance_user_task() {
        // 会签（并行多实例）：collection 用 flowable 属性，带 completionCondition。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="sign"/>
            <userTask id="sign" name="会签" flowable:assignee="${approver}">
              <multiInstanceLoopCharacteristics isSequential="false"
                   flowable:collection="approvers" flowable:elementVariable="approver">
                <completionCondition>${nrOfCompletedInstances/nrOfInstances &gt;= 0.5}</completionCondition>
              </multiInstanceLoopCharacteristics>
            </userTask>
            <sequenceFlow id="f1" sourceRef="sign" targetRef="e"/>
            <endEvent id="e"/>
          </process></definitions>"#;
        let def = compile(xml).expect("会签应能编译");
        let sign = def.node_by_bpmn("sign").unwrap();
        match &sign.kind {
            NodeKind::UserTask(ut) => {
                let mi = ut.multi_instance.as_ref().expect("应识别为多实例");
                assert!(!mi.sequential, "isSequential=false → 并行会签");
                assert_eq!(mi.collection_var, "approvers");
                assert_eq!(mi.element_var.as_deref(), Some("approver"));
                assert!(
                    mi.completion_condition
                        .as_deref()
                        .unwrap()
                        .contains("nrOfCompletedInstances"),
                    "应解析 completionCondition"
                );
            }
            other => panic!("sign 应为 userTask，实际 {other:?}"),
        }
    }

    #[test]
    fn compiles_sequential_multi_instance_with_loop_data_input_ref() {
        // 或签（顺序多实例）：collection 用 <loopDataInputRef> 子元素。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="chain"/>
            <userTask id="chain" name="或签">
              <multiInstanceLoopCharacteristics isSequential="true">
                <loopDataInputRef>approvers</loopDataInputRef>
                <completionCondition>${rejected == true}</completionCondition>
              </multiInstanceLoopCharacteristics>
            </userTask>
            <sequenceFlow id="f1" sourceRef="chain" targetRef="e"/>
            <endEvent id="e"/>
          </process></definitions>"#;
        let def = compile(xml).expect("或签应能编译");
        let chain = def.node_by_bpmn("chain").unwrap();
        match &chain.kind {
            NodeKind::UserTask(ut) => {
                let mi = ut.multi_instance.as_ref().expect("应识别为多实例");
                assert!(mi.sequential, "isSequential=true → 顺序或签");
                assert_eq!(
                    mi.collection_var, "approvers",
                    "应从 loopDataInputRef 取集合"
                );
                assert_eq!(mi.element_var, None);
            }
            other => panic!("chain 应为 userTask，实际 {other:?}"),
        }
    }

    #[test]
    fn plain_user_task_has_no_multi_instance() {
        let def = compile(SAMPLE).expect("样例应能编译");
        let review = def.node_by_bpmn("review").unwrap();
        match &review.kind {
            NodeKind::UserTask(ut) => assert!(ut.multi_instance.is_none(), "普通任务不应有 MI"),
            other => panic!("review 应为 userTask，实际 {other:?}"),
        }
    }

    #[test]
    fn compiles_interrupting_boundary_timer() {
        // 中断型边界定时器：经理审批超时 PT24H 自动升级。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="approve"/>
            <userTask id="approve" name="经理审批" flowable:assignee="manager"/>
            <sequenceFlow id="f1" sourceRef="approve" targetRef="done"/>
            <boundaryEvent id="timeout" attachedToRef="approve">
              <timerEventDefinition><timeDuration>PT24H</timeDuration></timerEventDefinition>
            </boundaryEvent>
            <sequenceFlow id="f2" sourceRef="timeout" targetRef="escalate"/>
            <userTask id="escalate" name="升级审批" flowable:assignee="director"/>
            <sequenceFlow id="f3" sourceRef="escalate" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let def = compile(xml).expect("边界定时器应能编译");
        let timeout = def.node_by_bpmn("timeout").unwrap();
        match &timeout.kind {
            NodeKind::BoundaryTimerEvent(bt) => {
                assert_eq!(bt.attached_to_bpmn_id, "approve");
                assert_eq!(bt.duration.seconds, 24 * 3600);
                assert!(bt.cancel_activity, "缺省应为中断型");
            }
            other => panic!("timeout 应为边界定时器，实际 {other:?}"),
        }
        // 边界事件挂在 approve 上，可被 boundary_timers_on 查到。
        assert_eq!(def.boundary_timers_on("approve").len(), 1);
        // 边界事件有一条出边（到 escalate）。
        assert_eq!(timeout.outgoing.len(), 1);
        assert_eq!(timeout.outgoing[0].target_bpmn_id, "escalate");
    }

    #[test]
    fn compiles_non_interrupting_boundary_timer() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="approve"/>
            <userTask id="approve" name="审批" flowable:assignee="mgr"/>
            <sequenceFlow id="f1" sourceRef="approve" targetRef="done"/>
            <boundaryEvent id="remind" attachedToRef="approve" cancelActivity="false">
              <timerEventDefinition><timeDuration>PT2H</timeDuration></timerEventDefinition>
            </boundaryEvent>
            <sequenceFlow id="f2" sourceRef="remind" targetRef="notify"/>
            <userTask id="notify" name="催办" flowable:assignee="assistant"/>
            <sequenceFlow id="f3" sourceRef="notify" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let def = compile(xml).expect("非中断边界定时器应能编译");
        let remind = def.node_by_bpmn("remind").unwrap();
        match &remind.kind {
            NodeKind::BoundaryTimerEvent(bt) => {
                assert!(!bt.cancel_activity, "cancelActivity=false → 非中断型");
                assert_eq!(bt.duration.seconds, 7200);
            }
            other => panic!("remind 应为边界定时器，实际 {other:?}"),
        }
    }

    #[test]
    fn rejects_non_timer_boundary_event() {
        // 非定时器边界事件（error）——M2.5 不支持，应报错。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
            <userTask id="t"/>
            <sequenceFlow id="f1" sourceRef="t" targetRef="done"/>
            <boundaryEvent id="err" attachedToRef="t">
              <errorEventDefinition/>
            </boundaryEvent>
            <sequenceFlow id="f2" sourceRef="err" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let err = compile(xml).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "非定时器边界事件应报不支持"
        );
    }

    #[test]
    fn compiles_candidate_expressions() {
        use cmx_flow_model::CandidateKind;
        // candidateUsers（User）+ candidateGroups（Role）+ 自定义 candidates（混合）。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn"
                     xmlns:cmx="http://cmx/flow">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
            <userTask id="t" name="审批"
                      flowable:candidateGroups="finance,legal"
                      cmx:candidates="position(cfo), org(d_fin)"
                      cmx:cc="user(u_boss)"/>
            <sequenceFlow id="f1" sourceRef="t" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let def = compile(xml).expect("候选人表达式应能编译");
        let t = def.node_by_bpmn("t").unwrap();
        match &t.kind {
            NodeKind::UserTask(ut) => {
                // 2 个 role（finance/legal）+ 1 个 position + 1 个 org = 4 条候选。
                assert_eq!(ut.candidates.len(), 4, "应汇总 4 条候选引用");
                assert_eq!(
                    ut.candidates
                        .iter()
                        .filter(|c| c.kind == CandidateKind::Role)
                        .count(),
                    2
                );
                assert!(
                    ut.candidates
                        .iter()
                        .any(|c| c.kind == CandidateKind::Position && c.value == "cfo")
                );
                assert!(
                    ut.candidates
                        .iter()
                        .any(|c| c.kind == CandidateKind::Org && c.value == "d_fin")
                );
                // 抄送：1 条 user。
                assert_eq!(ut.cc.len(), 1);
                assert_eq!(ut.cc[0].kind, CandidateKind::User);
                assert_eq!(ut.cc[0].value, "u_boss");
            }
            other => panic!("t 应为 userTask，实际 {other:?}"),
        }
    }

    #[test]
    fn plain_assignee_has_no_candidates() {
        // 纯静态 assignee（M1 老路）不产生候选引用。
        let def = compile(SAMPLE).expect("样例应能编译");
        let review = def.node_by_bpmn("review").unwrap();
        match &review.kind {
            NodeKind::UserTask(ut) => {
                assert!(ut.candidates.is_empty(), "静态 assignee 不应有候选引用");
                assert!(ut.cc.is_empty());
                assert_eq!(ut.assignee.as_deref(), Some("manager"));
            }
            other => panic!("review 应为 userTask，实际 {other:?}"),
        }
    }

    #[test]
    fn compiles_call_activity_with_var_mappings() {
        // callActivity：calledElement + 输入/输出变量映射（extensionElements 下 in/out）。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:flowable="http://flowable.org/bpmn">
          <process id="main" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="call"/>
            <callActivity id="call" name="财务复核" calledElement="fin_review">
              <extensionElements>
                <flowable:in source="amount" target="reviewAmount"/>
                <flowable:out source="approved" target="finApproved"/>
              </extensionElements>
            </callActivity>
            <sequenceFlow id="f1" sourceRef="call" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let def = compile(xml).expect("callActivity 应能编译");
        let call = def.node_by_bpmn("call").unwrap();
        match &call.kind {
            NodeKind::CallActivity(ca) => {
                assert_eq!(ca.called_element, "fin_review");
                assert!(ca.called_key.is_none());
                assert_eq!(ca.input_vars.len(), 1);
                assert_eq!(ca.input_vars[0].source, "amount");
                assert_eq!(ca.input_vars[0].target, "reviewAmount");
                assert_eq!(ca.output_vars.len(), 1);
                assert_eq!(ca.output_vars[0].source, "approved");
                assert_eq!(ca.output_vars[0].target, "finApproved");
            }
            other => panic!("call 应为 callActivity，实际 {other:?}"),
        }
        assert_eq!(call.outgoing.len(), 1, "callActivity 有一条出边");
    }

    #[test]
    fn call_activity_requires_target() {
        // 既无 calledElement 也无 calledKey → 报错。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="call"/>
            <callActivity id="call"/>
            <sequenceFlow id="f1" sourceRef="call" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        assert!(matches!(
            compile(xml).unwrap_err(),
            Error::MissingElement(_)
        ));
    }

    #[test]
    fn compiles_call_activity_with_logical_key() {
        // M5.2：cmx:calledKey 逻辑名（运行期由 SubflowRouter 按组织解析）。
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                     xmlns:cmx="http://cmx/flow">
          <process id="p" isExecutable="true">
            <startEvent id="s"/>
            <sequenceFlow id="f0" sourceRef="s" targetRef="call"/>
            <callActivity id="call" name="财务复核" cmx:calledKey="fin_review"/>
            <sequenceFlow id="f1" sourceRef="call" targetRef="done"/>
            <endEvent id="done"/>
          </process></definitions>"#;
        let def = compile(xml).expect("逻辑 key callActivity 应能编译");
        match &def.node_by_bpmn("call").unwrap().kind {
            NodeKind::CallActivity(ca) => {
                assert_eq!(ca.called_key.as_deref(), Some("fin_review"));
                assert!(
                    ca.called_element.is_empty(),
                    "用逻辑 key 时 calledElement 为空"
                );
            }
            other => panic!("call 应为 callActivity，实际 {other:?}"),
        }
    }
}
